"""Optional local Spotify transport for Lum Mix. Run with the existing Windows Python.

Uses Windows.Media.Control and aiohttp. Reads metadata only for the desktop Spotify app.
No playback is requested until an allowed mixer origin presents its status capability.
"""
import asyncio
import base64
from collections import OrderedDict
from concurrent.futures import ThreadPoolExecutor
import ctypes
from dataclasses import dataclass
import hashlib
import hmac
from io import BytesIO
import math
import os
import secrets
import time

from aiohttp import web
from mix_gains_http import MixOutput
from studio_preferences import StudioPreferences
from mic_routes import MicRoutes
from mix_duck_http import MixDucking
from spotify_duck_http import DUCK_METHODS, DuckRoutes, native_controller
from spotify_volume_http import VolumeError, VolumeRoutes
from spotify_window_transport import WindowPlayer, transport as window_transport
from spotify_artwork import ArtworkCache
from spotify_peak import SpotifyPeak
from spotify_locations import STORE_FAMILY, store_installed, website_installed

HOST = "127.0.0.1"
PORT = 41838
ORIGINS = {"http://127.0.0.1:41835", "http://localhost:41835"}

SPOTIFY_IDS = {"spotify.exe": "desktop", STORE_FAMILY.casefold() + "!spotify": "store"}
TOKEN_LIFETIME = 600
MAX_ART_BYTES = 4 * 1024 * 1024
MAX_ART_PIXELS = 12_000_000
CHANGE_WAIT = 22
CHANGE_DEBOUNCE = .055
TRANSPORT = {
    "play": ("is_play_enabled", "try_play_async", "canPlay"),
    "pause": ("is_pause_enabled", "try_pause_async", "canPause"),
    "previous": ("is_previous_enabled", "try_skip_previous_async", "canPrevious"),
    "next": ("is_next_enabled", "try_skip_next_async", "canNext"),
    "stop": ("is_stop_enabled", "try_stop_async", "canStop"),
}
SEEK_SECONDS = {"seekBack15": -15, "seekForward15": 15}


def status(state, **fields):
    return dict({"state": state, "playing": False, "canPlay": False, "canPause": False,
                 "canNext": False, "canPrevious": False, "canStop": False, "canSeek": False, "timeline": None,
                 "title": "", "artist": "", "token": None, "edition": "unknown",
                 "installations": [], "installationKnown": False, "artwork": None, "artworkId": None}, **fields)


def read_installations():
    editions = []
    complete = True
    for name, detect in (("desktop", website_installed), ("store", store_installed)):
        try:
            if detect():
                editions.append(name)
        except (ImportError, AttributeError, OSError, RuntimeError, NotImplementedError):
            complete = False
    return editions, complete


async def thumbnail_bytes(thumbnail):
    from winsdk.windows.storage.streams import DataReader
    with await thumbnail.open_read_async() as stream:
        size = stream.size
        if not 0 < size <= MAX_ART_BYTES:
            return None
        with DataReader(stream.get_input_stream_at(0)) as reader:
            if await reader.load_async(size) != size:
                return None
            data = bytearray(size)
            reader.read_bytes(data)
            return data


def read_session_media(session):
    # PortAudio can initialize the HTTP thread as STA. The thumbnail stream created
    # there needs a Windows message pump; asyncio alone does not dispatch those
    # messages. Acquire BOTH the properties and stream on our dedicated MTA thread.
    # Keep the original selected Spotify session even if another media app takes focus.
    async def read():
        props = await asyncio.wait_for(session.try_get_media_properties_async(), 2)
        labels = (props.title or "", props.artist or "", getattr(props, "album_title", ""),
                  getattr(props, "track_number", 0))
        raw = None
        thumb = getattr(props, "thumbnail", None)
        if thumb is not None:
            try:
                raw = await asyncio.wait_for(thumbnail_bytes(thumb), 1.5)
            except (ImportError, AttributeError, OSError, RuntimeError, TimeoutError):
                pass
        return labels, raw

    if os.name != "nt":
        return asyncio.run(read())
    runtime = ctypes.windll.combase
    result = runtime.RoInitialize(1)  # RO_INIT_MULTITHREADED; S_OK and S_FALSE both need uninitializing.
    if result < 0:
        raise OSError("Windows media apartment initialization failed")
    try:
        return asyncio.run(read())
    finally:
        runtime.RoUninitialize()


def encode_artwork(data):
    from PIL import Image
    try:
        with Image.open(BytesIO(data), formats=("JPEG", "PNG", "WEBP", "GIF")) as image:
            if image.width * image.height > MAX_ART_PIXELS:
                return None
            image.thumbnail((768, 768), Image.Resampling.LANCZOS)
            rgb = image.convert("RGB")
            # Preserve the artwork pixels while dropping source metadata and animation.
            rgb.info.clear()
            out = BytesIO()
            rgb.save(out, "JPEG", quality=85)
    except (Image.DecompressionBombError, ValueError, OSError, SyntaxError):
        return None
    return "data:image/jpeg;base64," + base64.b64encode(out.getvalue()).decode("ascii")


def spotify_sessions(manager):
    return [s for s in manager.get_sessions() if s.source_app_user_model_id.casefold() in SPOTIFY_IDS]


class WindowsSessions:
    async def manager(self):
        from winsdk.windows.media.control import GlobalSystemMediaTransportControlsSessionManager
        return await GlobalSystemMediaTransportControlsSessionManager.request_async()

    async def spotify(self):
        return spotify_sessions(await self.manager())

    @staticmethod
    def running_player():
        """Identify the local player even while Windows media controls are starting or unavailable."""
        from spotify_process_feed import AmbiguousTarget, spotify_target
        from spotify_volume_http import VolumeError, spotify_windows
        import win32gui

        try:
            target = spotify_target()
        except AmbiguousTarget:
            return {"state": "ambiguous"}
        if not target:
            return None
        pid, created, edition = target
        result = {"state": "detected", "edition": edition}
        try:
            windows = [hwnd for hwnd, owner, started in spotify_windows(edition)
                       if owner == pid and started == created]
            if len(windows) == 1:
                artist, separator, title = win32gui.GetWindowText(windows[0]).partition(" - ")
                if separator and title.strip() and artist.strip():
                    result.update(title=title.strip()[:256], artist=artist.strip()[:256])
                player = WindowPlayer(edition, (windows[0], pid, created))
                result.update(window_transport(player))
                if result.get("canPlay") or result.get("canPause"):
                    result.update(state="ready", transportSource="spotify_window", window=player)
        except (OSError, VolumeError, win32gui.error):
            pass
        return result


def session_timeline(session, info):
    """SMTC positions are samples at LastUpdatedTime, often several seconds old."""
    try:
        t = session.get_timeline_properties()
        start, end = t.start_time.total_seconds(), t.end_time.total_seconds()
        if end <= start:
            return None
        lo = max(start, t.min_seek_time.total_seconds())
        hi = min(end, t.max_seek_time.total_seconds())
        pos, now = t.position.total_seconds(), time.time()
        rate = getattr(info, "playback_rate", None)
        rate = float(rate) if rate is not None and math.isfinite(rate) and rate > 0 else 1.0
        if info.playback_status.name == "PLAYING" and t.last_updated_time.year >= 1970:
            pos += max(0, now-t.last_updated_time.timestamp()) * rate
        return {"start": start, "end": end, "position": max(start, min(pos, end)), "rate": rate, "sampledAt": now,
                "minSeek": lo if hi > lo else None, "maxSeek": hi if hi > lo else None}
    except (AttributeError, OSError, RuntimeError, OverflowError, ValueError):
        return None


def can_seek(controls, timeline):
    return bool(getattr(controls, "is_playback_position_enabled", False)
                and timeline is not None and timeline["minSeek"] is not None)


class ChangeFeed:
    """One set of native listeners, shared by all mixer windows."""
    def __init__(self, sessions):
        self.sessions = sessions
        self.prefix = secrets.token_hex(8)
        self.sequence = 0
        self.wakeup = asyncio.Event()
        self.lock = asyncio.Lock()
        self.loop = self.manager = self.manager_token = self.timer = None
        self.bindings = []
        self.refresh_sessions = False
        self.retry_after = 0
        self.closed = False

    @property
    def revision(self):
        return f"{self.prefix}.{self.sequence}"

    def notify(self):
        self.sequence += 1
        previous, self.wakeup = self.wakeup, asyncio.Event()
        previous.set()

    def dispatch(self, refresh):
        # WinRT delivers callbacks on its own threads. All subscription state,
        # debounce timers and HTTP waiters belong to the aiohttp event loop.
        if not self.closed and self.loop and not self.loop.is_closed():
            self.loop.call_soon_threadsafe(self.schedule, refresh)

    def schedule(self, refresh):
        if self.closed or self.manager_token is None:
            return
        self.refresh_sessions |= refresh
        if self.timer is None:
            self.timer = self.loop.call_later(CHANGE_DEBOUNCE, self.flush)

    def session_event(self, sender, args):
        self.dispatch(False)

    def manager_event(self, sender, args):
        self.dispatch(True)

    @staticmethod
    def unsubscribe(bindings):
        for remove, token in bindings:
            try:
                remove(token)
            except (OSError, RuntimeError):
                pass  # The session may already be gone.

    def bind_sessions(self):
        new = []
        try:
            for session in spotify_sessions(self.manager):
                for add, remove in ((session.add_media_properties_changed, session.remove_media_properties_changed),
                                    (session.add_playback_info_changed, session.remove_playback_info_changed)):
                    new.append((remove, add(self.session_event)))
        except Exception:
            self.unsubscribe(new)
            raise
        self.unsubscribe(self.bindings)
        self.bindings = new

    def disconnect(self):
        if self.timer is not None:
            self.timer.cancel()
            self.timer = None
        self.refresh_sessions = False
        self.unsubscribe(self.bindings)
        self.bindings = []
        if self.manager_token is not None:
            self.unsubscribe([(self.manager.remove_sessions_changed, self.manager_token)])
        self.manager = self.manager_token = None
        self.retry_after = time.monotonic() + 30

    def flush(self):
        self.timer = None
        if self.refresh_sessions:
            self.refresh_sessions = False
            had_spotify = bool(self.bindings)
            try:
                self.bind_sessions()
                if not self.bindings and not had_spotify:
                    return
            except (AttributeError, TypeError, OSError, RuntimeError):
                self.disconnect()
        self.notify()

    async def ensure(self):
        async with self.lock:
            if self.closed or self.manager_token is not None or time.monotonic() < self.retry_after:
                return
            self.loop = asyncio.get_running_loop()
            try:
                self.manager = await asyncio.wait_for(self.sessions.manager(), 3)
                if self.closed:
                    return
                self.manager_token = self.manager.add_sessions_changed(self.manager_event)
                self.bind_sessions()
                self.notify()
            except (ImportError, AttributeError, OSError, RuntimeError, TypeError, TimeoutError):
                self.disconnect()

    async def changes(self, since):
        await self.ensure()
        if self.manager_token is not None and since == self.revision:
            # The revision check and capture of its event have no intervening await.
            # Each revision broadcasts to all waiters; late callers compare revisions.
            try:
                await asyncio.wait_for(self.wakeup.wait(), CHANGE_WAIT)
            except TimeoutError:
                pass
        return {"revision": self.revision, "events": self.manager_token is not None}

    async def close(self, app):
        self.closed = True
        self.disconnect()
        self.notify()


@dataclass
class Selection:
    session: object
    origin: str
    expires: float


class Player:
    def __init__(self, sessions, inventory=read_installations):
        self.sessions = sessions
        self.tokens = OrderedDict()
        self.lock = asyncio.Lock()
        self.inventory_lock = asyncio.Lock()
        self.inventory = inventory
        self.installed = ([], False)
        self.inventory_expires = 0
        self.art_key = self.art_value = None
        self.spotify_covers = ArtworkCache(encode_artwork)
        self.art_lock = asyncio.Lock()
        self.media_worker = ThreadPoolExecutor(max_workers=1, thread_name_prefix="LumSpotifyMedia")

    async def close(self, app):
        await asyncio.to_thread(self.media_worker.shutdown, wait=True, cancel_futures=True)

    async def artwork(self, labels, data, edition):
        if not data:
            self.art_key = self.art_value = None
            return None
        try:
            # Different recordings can share every published text field. Fingerprint the
            # current bytes too, and keep only one entry, including unsupported artwork.
            key = (edition, labels, hashlib.sha256(data).hexdigest())
            async with self.art_lock:
                if key != self.art_key:
                    value = await asyncio.wait_for(asyncio.to_thread(encode_artwork, data), 1.5)
                    self.art_key, self.art_value = key, (key[-1][:24], value) if value else None
                return self.art_value
        except (ImportError, AttributeError, OSError, RuntimeError, TimeoutError):
            return None

    async def installation_status(self):
        async with self.inventory_lock:
            if time.monotonic() >= self.inventory_expires:
                self.installed = await asyncio.to_thread(self.inventory)
                self.inventory_expires = time.monotonic() + 30
        editions, complete = self.installed
        return {"installations": editions, "installationKnown": complete}

    async def selected(self):
        try:
            choices = await asyncio.wait_for(self.sessions.spotify(), 3)
        except (ImportError, NotImplementedError):
            return None, "unsupported"
        except (OSError, RuntimeError, TimeoutError):
            return None, "unavailable"
        if not choices:
            return None, "no_session"
        if len(choices) == 1:
            return choices[0], "ready"
        try:
            playing = [s for s in choices if s.get_playback_info().playback_status.name == "PLAYING"]
        except (AttributeError, OSError, RuntimeError):
            return None, "ambiguous"
        return (playing[0], "ready") if len(playing) == 1 else (None, "ambiguous")

    def issue(self, session, origin):
        now = time.monotonic()
        while self.tokens and (next(iter(self.tokens.values())).expires <= now or len(self.tokens) >= 64):
            self.tokens.popitem(last=False)
        token = secrets.token_urlsafe(32)
        self.tokens[token] = Selection(session, origin, now + TOKEN_LIFETIME)
        return token

    async def snapshot(self, origin, previous_artwork_id=None):
        (session, state), installed = await asyncio.gather(self.selected(), self.installation_status())
        if session is None:
            self.art_key = self.art_value = None
            if state != "ambiguous":
                detect = getattr(self.sessions, "running_player", None)
                try:
                    running = await asyncio.to_thread(detect) if detect else None
                except (ImportError, OSError, RuntimeError):
                    running = None
                if running:
                    fields = dict(running)
                    window = fields.pop("window", None)
                    if window is not None:
                        fields["token"] = self.issue(window, origin)
                    if fields.get("itemUrl"):
                        cover = await self.spotify_covers.get(fields["itemUrl"])
                        if cover:
                            fields["artworkId"], fields["artwork"] = cover
                            if previous_artwork_id == cover[0]:
                                reply = status(**fields, **installed)
                                del reply["artwork"]
                                return reply
                    return status(**fields, **installed)
            return status(state, **installed)
        edition = SPOTIFY_IDS[session.source_app_user_model_id.casefold()]
        try:
            info = session.get_playback_info()
            controls = info.controls
            playing = info.playback_status.name == "PLAYING"
            capability = {field: bool(getattr(controls, flag, False)) for flag, _, field in TRANSPORT.values()}
            timeline = session_timeline(session, info)
        except (AttributeError, OSError, RuntimeError):
            return status("unavailable", edition=edition, **installed)
        title = artist = ""
        artwork = None
        try:
            labels, data = await asyncio.get_running_loop().run_in_executor(self.media_worker, read_session_media, session)
            title, artist = labels[0][:256], labels[1][:256]
            artwork = await self.artwork(labels, data, edition)
        except (AttributeError, OSError, RuntimeError, TimeoutError):
            pass  # Transport can work even when the player publishes no track metadata.
        item_url = None
        if not artwork and title and hasattr(self.sessions, "running_player"):
            try:
                window = await asyncio.to_thread(self.sessions.running_player)
                if (window and window.get("edition") == edition and window.get("title", "").casefold() == title.casefold()
                        and artist and window.get("artist", "").casefold() == artist.casefold()):
                    item_url = window.get("itemUrl")
                    artwork = await self.spotify_covers.get(item_url)
            except (ImportError, OSError, RuntimeError):
                pass
        artwork_id, url = artwork or (None, None)
        result = status("ready", playing=playing, canSeek=can_seek(controls,timeline), timeline=timeline, **capability,
                      title=title, artist=artist, token=self.issue(session, origin), edition=edition,
                      artwork=url, artworkId=artwork_id, itemUrl=item_url, **installed)
        if artwork_id and previous_artwork_id == artwork_id:
            del result["artwork"]
        return result

    async def validate_duck_start(self, origin, token):
        selection = self.tokens.get(token)
        if not selection or selection.expires <= time.monotonic() or not hmac.compare_digest(selection.origin, origin):
            return 403, "refresh_selection"
        if isinstance(selection.session, WindowPlayer):
            return 409, "windows_media_unavailable"
        current, _ = await self.selected()
        if current is None or current.source_app_user_model_id.casefold() != selection.session.source_app_user_model_id.casefold():
            return 409, "selection_changed"
        return 200, None

    async def control(self, origin, token, action):
        async with self.lock:
            selected = self.tokens.get(token)
            if not selected or selected.expires <= time.monotonic() or not hmac.compare_digest(selected.origin, origin):
                return 403, status("unavailable", error="refresh_selection")
            if isinstance(selected.session, WindowPlayer):
                try:
                    await asyncio.to_thread(window_transport, selected.session, action)
                except VolumeError as error:
                    return error.http, status("unavailable", error=error.reason)
                return 200, await self.snapshot(origin)
            current, state = await self.selected()
            if current is None:
                self.tokens.pop(token, None)
                return 409, status(state, error="selection_changed")
            # Dispatch to the exact originally selected native session, never the current system player.
            if current.source_app_user_model_id.casefold() != selected.session.source_app_user_model_id.casefold():
                self.tokens.pop(token, None)
                return 409, status("unavailable", error="selection_changed")
            session = selected.session
            try:
                info = session.get_playback_info()
                if current.get_playback_info().playback_status != info.playback_status:
                    self.tokens.pop(token, None)
                    return 409, status("unavailable", error="selection_changed")
                playing = info.playback_status.name == "PLAYING"
                if (action == "play" and playing) or (action == "pause" and info.playback_status.name == "PAUSED"):
                    return 200, await self.snapshot(origin)
                if action in SEEK_SECONDS:
                    timeline = session_timeline(session, info)
                    if not can_seek(info.controls, timeline):
                        return 409, dict(await self.snapshot(origin), error="control_disabled")
                    target = max(timeline["minSeek"], min(timeline["maxSeek"], timeline["position"] + SEEK_SECONDS[action]))
                    # Windows expects an absolute signed TimeSpan count in 100 ns units.
                    operation = session.try_change_playback_position_async(round(target * 10_000_000))
                else:
                    flag, method, _ = TRANSPORT[action]
                    if not getattr(info.controls, flag, False):
                        return 409, dict(await self.snapshot(origin), error="control_disabled")
                    operation = getattr(session, method)()
                accepted = await asyncio.wait_for(operation, 3)
                if not accepted:
                    return 409, dict(await self.snapshot(origin), error="player_declined")
            except (AttributeError, OSError, RuntimeError, TimeoutError):
                return 503, status("unavailable", error="check_player")
            return 200, await self.snapshot(origin)


def create_app(sessions=None, *, expected_host=f"{HOST}:{PORT}", inventory=read_installations,
               duck_factory=None, mix_output=None, peak_meter=None):
    player = Player(sessions if sessions is not None else WindowsSessions(), inventory)
    feed = ChangeFeed(player.sessions)
    peak_meter = peak_meter if peak_meter is not None else SpotifyPeak()
    allowed_methods = {path: (method,) for path, method in DUCK_METHODS.items()} if duck_factory is not None else {}
    allowed_methods["/lummix-health"] = ("GET",)
    allowed_methods["/mix/duck"] = ("GET", "POST")
    allowed_methods["/mix/preferences"] = ("GET", "POST")
    allowed_methods.update({"/spotify/status": ("GET",), "/spotify/control": ("POST",),
                            "/spotify/changes": ("GET",), "/spotify/peak": ("GET",), "/spotify/volume": ("GET", "POST"),
                            "/mix/mic": ("GET", "POST"), "/mix/gains": ("GET", "POST")})

    @web.middleware
    async def restrict(request, handler):
        origin = request.headers.get("Origin", "")
        if (request.headers.getall("Host", []) != [expected_host]
                or request.headers.getall("Origin", []) != [origin] or origin not in ORIGINS):
            return web.json_response({"error": "unauthorized_origin"}, status=403)
        if request.path not in allowed_methods:
            return web.Response(status=404)
        if request.method == "OPTIONS":
            methods = allowed_methods[request.path]
            response = web.Response(status=204 if request.headers.get("Access-Control-Request-Method") in methods else 405)
            response.headers.update({"Access-Control-Allow-Methods": ", ".join(methods),
                                     "Access-Control-Allow-Headers": "Content-Type, X-Lum-Spotify-Token, X-Lum-Duck-Token, X-Lum-Mix-Token"})
        else:
            response = await handler(request)
        response.headers.update({"Access-Control-Allow-Origin": origin, "Vary": "Origin",
                                 "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff"})
        return response

    async def health(request):
        # Report API contracts, independent of whether Spotify or a physical mic is connected.
        paths = {r.resource.canonical for r in request.app.router.routes()}
        caps = ["spotify-detection-v2", "spotify-window-artwork-v1"]
        if "/spotify/volume" in paths:
            caps.append("spotify-player-volume-v1")
        if "/spotify/peak" in paths:
            caps.append("spotify-session-peak-v1")
        from spotify_process_feed import supported as process_mix_supported
        if process_mix_supported():
            caps.append("spotify-process-mix-v1")
        if "/mix/duck" in paths:
            caps.append("mixer-duck-v1")
        if "/mix/mic" in paths:
            caps.extend(("mic-route-v1","mix-setup-alias-v1","cable-restart-v1"))
        if "/mix/preferences" in paths:
            caps.append("studio-preferences-v1")
        return web.json_response({"service":"lummix-media","protocol":2,"capabilities":caps})

    async def query(request):
        return web.json_response(await player.snapshot(request.headers["Origin"], request.query.get("artworkId")))

    async def changes(request):
        return web.json_response(await feed.changes(request.query.get("since")))

    async def read_peak(request):
        origin = request.headers["Origin"]
        token = request.headers.get("X-Lum-Spotify-Token", "")
        selection = player.tokens.get(token)
        if selection and isinstance(selection.session, WindowPlayer):
            if selection.expires <= time.monotonic() or not hmac.compare_digest(selection.origin, origin):
                return web.json_response({"state": "unavailable", "error": "refresh_selection"}, status=403)
            try:
                from spotify_volume_http import spotify_windows
                window = selection.session
                if await asyncio.to_thread(spotify_windows, window.edition) != [window.identity]:
                    return web.json_response({"state": "unavailable", "error": "selection_changed"}, status=409)
                level = await peak_meter.read(window.edition)
            except (ImportError, OSError, RuntimeError, VolumeError):
                level = None
            return web.json_response({"state":"no_local_signal"} if level is None else
                                     {"state":"ready", "peak":round(level, 3)})
        code, error = await player.validate_duck_start(origin, token)
        if error:
            return web.json_response({"state": "unavailable", "error": error}, status=code)
        session = selection.session
        edition = SPOTIFY_IDS[session.source_app_user_model_id.casefold()]
        try:
            if session.get_playback_info().playback_status.name != "PLAYING":
                return web.json_response({"state": "paused"})
            level = await peak_meter.read(edition)
        except (AttributeError, ImportError, OSError, RuntimeError):
            level = None
        if level is None:
            return web.json_response({"state": "no_local_signal"})
        return web.json_response({"state": "ready", "peak": round(level, 3)})

    async def command(request):
        token = request.headers.get("X-Lum-Spotify-Token", "")
        if not token or len(token) != 43:
            return web.json_response(status("unavailable", error="refresh_selection"), status=403)
        if request.content_type != "application/json":
            return web.Response(status=415)
        try:
            body = await request.json()
        except (ValueError, UnicodeError):
            return web.Response(status=400)
        if (not isinstance(body, dict) or set(body) != {"action"} or not isinstance(body["action"], str)
                or body["action"] not in TRANSPORT and body["action"] not in SEEK_SECONDS):
            return web.Response(status=400)
        code, result = await player.control(request.headers["Origin"], token, body["action"])
        return web.json_response(result, status=code)

    app = web.Application(middlewares=[restrict], client_max_size=256)
    app.router.add_get("/lummix-health", health, allow_head=False)
    app.router.add_get("/spotify/status", query, allow_head=False)
    app.router.add_get("/spotify/changes", changes, allow_head=False)
    app.router.add_get("/spotify/peak", read_peak, allow_head=False)
    app.router.add_post("/spotify/control", command)
    output = mix_output if mix_output is not None else MixOutput()
    output.install(app)
    StudioPreferences(output).install(app)
    mic = MicRoutes(output)
    mic.install(app)
    def route_level():
        if not mic.active:
            return None
        mixer = output.mixer
        if mixer is None or mixer.stream is None:
            return None
        try:
            if not mixer.stream.active:
                return None
        except (OSError, RuntimeError):
            return None
        if mixer.controls.mute_master or mixer.controls.mute_mic:
            return 0.
        return min(1., max(0., mixer.mic_rms))
    VolumeRoutes(player, SPOTIFY_IDS).install(app)
    MixDucking(output, mic).install(app)
    if duck_factory is not None:
        DuckRoutes(player.validate_duck_start, duck_factory, route_level).install(app)
    app.on_shutdown.append(feed.close)
    app.on_shutdown.append(player.close)
    app.on_shutdown.append(peak_meter.close)
    return app


if __name__ == "__main__":
    web.run_app(create_app(), host=HOST, port=PORT, access_log=None, print=None)

