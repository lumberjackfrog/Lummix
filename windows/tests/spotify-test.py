"""HTTP acceptance using fake media sessions only. This test never opens a Windows player."""
import importlib.util
import asyncio
import base64
from datetime import datetime, timedelta, timezone
from io import BytesIO
from pathlib import Path
import sys
import ctypes
import os
import threading
import time
from types import SimpleNamespace as NS
import unittest
from unittest.mock import AsyncMock, MagicMock, Mock, call, patch
from PIL import Image

from aiohttp.test_utils import TestClient, TestServer
import spotify_artwork
from spotify_window_transport import artwork_bar, now_playing

spec = importlib.util.spec_from_file_location("spotify_helper", Path(__file__).resolve().parents[1]/"spotify-helper.py")
mod = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = mod
spec.loader.exec_module(mod)


SPOTIFY_OPEN = "https://open.spotify.com"
TRACK = f"{SPOTIFY_OPEN}/track/{'A' * 22}"
ALBUM = f"{SPOTIFY_OPEN}/album/{'B' * 22}"


def cover_png(color):
    out = BytesIO()
    with Image.new("RGB", (30, 30), color) as image:
        image.save(out, format="PNG")
    return out.getvalue()


class ArtworkTests(unittest.IsolatedAsyncioTestCase):
    def test_spotify_item_requires_an_exact_https_item(self):
        for item in (TRACK, ALBUM, f"{SPOTIFY_OPEN}/episode/{'C' * 22}"):
            with self.subTest(item=item):
                raw = item.replace("open.spotify.com", "xpui.app.spotify.com") + "?si=opaque"
                self.assertEqual(spotify_artwork.spotify_item(raw), item)
        invalid = (
            None, "spotify:track:" + "A" * 22, TRACK.replace("https:", "http:"),
            TRACK.replace(".com/", ".com.invalid/"), TRACK.replace(".com/", ".com@invalid/"),
            TRACK.replace(".com/", ".com:443/"), TRACK.replace("track", "playlist"),
            TRACK + "/", TRACK + "#fragment", TRACK[:-1], TRACK + "A",
        )
        for raw in invalid:
            with self.subTest(raw=raw):
                self.assertIsNone(spotify_artwork.spotify_item(raw))

    def test_now_playing_uses_only_unique_item_in_the_player_region(self):
        def elements(*items):
            return NS(Length=len(items), GetElement=lambda index: items[index])

        def link(name, url):
            return NS(CurrentName=name, GetCurrentPropertyValue=Mock(return_value=url))

        uia = NS(TreeScope_Children=1, UIA_AriaRolePropertyId=2, UIA_HyperlinkControlTypeId=3,
                 UIA_ValueValuePropertyId=4)
        region = object()
        bar = NS(CurrentAriaRole="complementary", FindAll=Mock(return_value=elements(region)))
        ui = NS(CreatePropertyCondition=Mock(return_value="region condition"))
        title = link(" Fixture track ", ALBUM.replace("open.spotify.com", "xpui.app.spotify.com"))
        artist = link("Fixture artist", f"{SPOTIFY_OPEN}/artist/{'D' * 22}")
        unrelated = link("Another title", TRACK.replace("open.spotify.com", "unrelated.invalid"))
        with patch("spotify_window_transport.controls", return_value=[title, artist, unrelated]) as controls:
            self.assertEqual(now_playing(ui, uia, bar),
                             {"itemUrl": ALBUM, "title": "Fixture track", "artist": "Fixture artist"})
            bar.FindAll.assert_called_once_with(uia.TreeScope_Children, "region condition")
            controls.assert_called_once_with(ui, uia, region, uia.UIA_HyperlinkControlTypeId)
            controls.return_value.append(link("Second item", TRACK))
            self.assertEqual(now_playing(ui, uia, bar), {})
            controls.reset_mock()
            bar.FindAll.return_value = elements(region, object())
            self.assertEqual(now_playing(ui, uia, bar), {})
            controls.assert_not_called()

    def test_artwork_bar_requires_one_player_with_a_seek_slider(self):
        uia = NS(TreeScope_Children=1, TreeScope_Descendants=2, UIA_AriaRolePropertyId=3,
                 UIA_GroupControlTypeId=4, UIA_SliderControlTypeId=5)
        ui = NS(CreatePropertyCondition=Mock(return_value="bar condition"))
        sidebar, player, seek_group = object(), object(), object()
        bars = (sidebar, player)
        root = NS(FindAll=Mock(return_value=NS(Length=len(bars), GetElement=bars.__getitem__)))
        controls_by_root = {sidebar: [], player: [seek_group], seek_group: [object()]}
        metadata = {"itemUrl": TRACK, "title": "Track", "artist": "Artist"}
        with patch("spotify_window_transport.controls", side_effect=lambda ui, uia, root, kind, scope=None:
                   controls_by_root[root]), patch("spotify_window_transport.now_playing", return_value=metadata) as read:
            self.assertEqual(artwork_bar(ui, uia, root), metadata)
            root.FindAll.assert_called_once_with(uia.TreeScope_Descendants, "bar condition")
            read.assert_called_once_with(ui, uia, player)
            controls_by_root[sidebar] = [seek_group]
            self.assertEqual(artwork_bar(ui, uia, root), {})

    async def test_oembed_only_fetches_allowed_thumbnail_urls_without_redirects(self):
        endpoint = f"{SPOTIFY_OPEN}/oembed"
        thumbnail = "https://i.scdn.co/image/FixtureCover1"
        session = MagicMock()
        session.__aenter__.return_value = session
        session.get.return_value.__aenter__.return_value = object()
        with patch.object(spotify_artwork.aiohttp, "ClientSession", return_value=session), \
             patch.object(spotify_artwork, "read_limited", new_callable=AsyncMock) as read:
            read.side_effect = [b'{"thumbnail_url":"https://i.scdn.co/image/FixtureCover1"}', b"cover"]
            self.assertEqual(await spotify_artwork.fetch_cover(TRACK), b"cover")
            self.assertEqual(session.get.call_args_list,
                             [call(endpoint, params={"url": TRACK}, allow_redirects=False),
                              call(thumbnail, allow_redirects=False)])
            for value in ("https://i.scdn.co.invalid/image/FixtureCover1", "https://i.scdn.co@invalid/image/FixtureCover1",
                          "https://i.scdn.co/image/not/a/thumbnail", "http://i.scdn.co/image/FixtureCover1"):
                with self.subTest(value=value):
                    session.get.reset_mock()
                    read.side_effect = [f'{{"thumbnail_url":"{value}"}}'.encode()]
                    with self.assertRaises(ValueError):
                        await spotify_artwork.fetch_cover(TRACK)
                    session.get.assert_called_once_with(endpoint, params={"url": TRACK}, allow_redirects=False)

    async def test_failed_cover_retries_without_repeated_fetches(self):
        cache = spotify_artwork.ArtworkCache(lambda _: "encoded cover")
        clock = NS(monotonic=Mock(return_value=5))
        with patch.object(spotify_artwork, "time", clock), \
             patch.object(spotify_artwork, "fetch_cover", side_effect=[TimeoutError(), b"a", b"b"]) as fetch:
            self.assertIsNone(await cache.get(TRACK))
            self.assertIsNone(await cache.get(TRACK + "?si=same-item"))
            fetch.assert_awaited_once_with(TRACK)
            clock.monotonic.return_value += 21
            first, duplicate = await asyncio.gather(cache.get(TRACK), cache.get(TRACK))
            self.assertEqual(first, duplicate)
            self.assertEqual(first[1], "encoded cover")
            self.assertNotEqual(first, await cache.get(ALBUM))
            self.assertEqual(fetch.await_args_list, [call(TRACK), call(TRACK), call(ALBUM)])


class Session:
    def __init__(self, app_id):
        self.source_app_user_model_id = app_id
        self.playing = False
        self.enabled = True
        self.accept = True
        self.calls = []
        self.thumbnail = None
        self.media_handlers = {}
        self.playback_handlers = {}
        self.metadata_reads = 0
        self.transport = dict(is_next_enabled=False, is_previous_enabled=False, is_stop_enabled=False,
                              is_playback_position_enabled=False)
        self.timeline = None
        self.rate = None

    def add_media_properties_changed(self, callback):
        token = object()
        self.media_handlers[token] = callback
        return token

    def remove_media_properties_changed(self, token):
        del self.media_handlers[token]

    def add_playback_info_changed(self, callback):
        token = object()
        self.playback_handlers[token] = callback
        return token

    def remove_playback_info_changed(self, token):
        del self.playback_handlers[token]

    def emit(self, callbacks):
        for callback in list(callbacks.values()):
            callback(self, None)

    def get_playback_info(self):
        return NS(playback_status=NS(name="PLAYING" if self.playing else "PAUSED"),
                  playback_rate=self.rate,
                  controls=NS(is_play_enabled=self.enabled and not self.playing,
                              is_pause_enabled=self.enabled and self.playing, **self.transport))

    def get_timeline_properties(self):
        return self.timeline

    async def try_skip_previous_async(self):
        self.calls.append("previous")
        return self.accept

    async def try_skip_next_async(self):
        self.calls.append("next")
        return self.accept

    async def try_stop_async(self):
        self.calls.append("stop")
        self.playing = False
        return self.accept

    async def try_change_playback_position_async(self, ticks):
        self.calls.append(("seek", ticks))
        self.timeline.position = timedelta(microseconds=ticks/10)
        self.timeline.last_updated_time = datetime.now(timezone.utc)
        return self.accept

    async def try_get_media_properties_async(self):
        self.metadata_reads += 1
        return NS(title="Fixture track", artist="Fixture artist", thumbnail=self.thumbnail)

    async def try_play_async(self):
        self.calls.append("play")
        self.playing = self.accept
        return self.accept

    async def try_pause_async(self):
        self.calls.append("pause")
        self.playing = not self.accept
        return self.accept


class Sessions:
    def __init__(self, items):
        self.items = items
        self.failure = None
        self.manager_listener = None

    async def manager(self):
        if self.failure:
            raise self.failure
        return self

    def get_sessions(self):
        return self.items

    def add_sessions_changed(self, callback):
        self.manager_listener = callback
        return 0

    def remove_sessions_changed(self, token):
        self.manager_listener = None

    async def spotify(self):
        if self.failure:
            raise self.failure
        return mod.spotify_sessions(NS(get_sessions=lambda: self.items))


class FixtureDucker:
    def __init__(self):
        self.started = False
        self.fail = None
        self.feeds = []
        self.stops = 0
        self.depth = 0

    def start_external(self, depth):
        if self.fail:
            raise self.fail
        self.depth, self.started = depth, True

    def set_depth(self, depth):
        self.depth = depth

    def feed_mic_rms(self, rms):
        self.feeds.append(rms)

    def status(self):
        return {"state": "active" if self.started else "off", "sessions": int(self.started), "reductionDb": 0}

    def stop(self):
        self.stops += 1
        self.started = False


class Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.spotify = Session("Spotify.exe")
        self.browser = Session("Microsoft.Edge")
        self.sessions = Sessions([self.browser, self.spotify])
        self.installed = (["desktop"], True)
        self.ducker = FixtureDucker()
        self.peak = NS(read=AsyncMock(return_value=.3426), close=AsyncMock())
        self.client = TestClient(TestServer(mod.create_app(self.sessions, inventory=lambda: self.installed,
                                                          duck_factory=lambda: self.ducker, peak_meter=self.peak)))
        await self.client.start_server()
        self.headers = {"Origin":"http://127.0.0.1:41835", "Host":"127.0.0.1:41838"}

    async def asyncTearDown(self):
        await self.client.close()
        self.assertFalse(self.browser.calls)

    async def get(self, artwork_id=None):
        params = {"artworkId": artwork_id} if artwork_id else {}
        response = await self.client.get("/spotify/status", headers=self.headers, params=params)
        self.assertEqual(response.status,200)
        return await response.json()

    async def post(self,token,action,**headers):
        h={**self.headers,"X-Lum-Spotify-Token":token,**headers}
        return await self.client.post("/spotify/control",headers=h,json={"action":action})

    async def duck(self, route, token, body, **overrides):
        header = "X-Lum-Spotify-Token" if route == "start" else "X-Lum-Duck-Token"
        return await self.client.post("/duck/"+route, headers={**self.headers,header:token,**overrides}, json=body)

    async def test_peak_is_authenticated_and_requires_playing(self):
        path = "/spotify/peak"
        blocked = await self.client.get(path, headers=self.headers)
        self.assertEqual(blocked.status, 403)
        token = (await self.get())["token"]
        headers = {**self.headers, "X-Lum-Spotify-Token": token}
        paused = await self.client.get(path, headers=headers)
        self.assertEqual(await paused.json(), {"state": "paused"})
        self.peak.read.assert_not_called()
        self.spotify.playing = True
        playing = await self.client.get(path, headers=headers)
        self.assertEqual(await playing.json(), {"state": "ready", "peak": .343})
        self.peak.read.assert_awaited_once_with("desktop")
        self.peak.read.return_value = None
        remote = await self.client.get(path, headers=headers)
        self.assertEqual(await remote.json(), {"state": "no_local_signal"})
        denied = await self.client.post(path, headers=headers)
        self.assertEqual(denied.status, 405)
        self.assertFalse(self.spotify.calls)

    async def test_duck_explicit_capabilities_and_restoration(self):
        init = await self.client.get("/duck/status",headers=self.headers)
        self.assertEqual((await init.json())["state"],"off")
        player = (await self.get())["token"]
        self.assertEqual((await self.duck("start","x"*43,{"depthDb":12})).status,403)
        r = await self.duck("start",player,{"depthDb":12})
        self.assertEqual(r.status,200)
        handle = (await r.json())["duckToken"]
        self.assertNotEqual(handle,player)
        self.assertEqual((await self.duck("start",(await self.get())["token"],{"depthDb":12})).status,409)
        self.assertEqual((await self.duck("level",player,{"rms":.1,"depthDb":8})).status,403)
        self.assertEqual((await self.duck("level",handle,{"rms":.2,"depthDb":8},Origin="http://localhost:41835")).status,403)
        self.assertEqual((await self.duck("level",handle,{"rms":.3,"depthDb":0})).status,200)
        self.assertEqual((self.ducker.feeds,self.ducker.depth),([.3],0))
        self.assertEqual((await self.duck("stop",handle,{})).status,200)
        self.assertEqual(self.ducker.stops,1)
        self.assertEqual((await self.duck("level",handle,{"rms":.3,"depthDb":2})).status,403)
        self.assertFalse(self.spotify.calls)

    async def test_duck_reuses_selected_native_mic_and_releases_on_disconnect(self):
        mic = NS(active="fixture mic", install=lambda app:None)
        from tempfile import TemporaryDirectory
        from cable_mixer import CableMixer
        from mix_gains_http import MixOutput
        mixer=CableMixer()
        mixer.stream,mixer.mic_rms=NS(active=True),.23
        output=MixOutput(Path(self.enterContext(TemporaryDirectory()))/"gains.json",mixer)
        with patch.object(mod, "MicRoutes", return_value=mic):
            app=mod.create_app(self.sessions,inventory=lambda:self.installed,mix_output=output,
                               duck_factory=lambda:self.ducker,peak_meter=self.peak)
        async with TestClient(TestServer(app)) as client:
            status=await (await client.get("/duck/status",headers=self.headers)).json()
            self.assertTrue(status["micRouteActive"])
            player=await (await client.get("/spotify/status",headers=self.headers)).json()
            started=await client.post("/duck/start",headers=self.headers|{"X-Lum-Spotify-Token":player["token"]},
                                      json={"depthDb":12,"source":"route"})
            self.assertEqual(started.status,200)
            headers=self.headers|{"X-Lum-Duck-Token":(await started.json())["duckToken"]}
            async def heartbeat():
                return await client.post("/duck/level",headers=headers,json={"depthDb":12})
            self.assertEqual((await heartbeat()).status,200)
            mixer.set_controls(mute_mic=True)
            self.assertEqual((await heartbeat()).status,200)
            self.assertEqual(self.ducker.feeds,[.23,0])
            mic.active=None
            stopped=await heartbeat()
            self.assertEqual(stopped.status,409)
            self.assertEqual((await stopped.json())["error"],"mic_route_stopped")
            self.assertEqual(self.ducker.stops,1)

    async def test_duck_lease_expiry_and_shutdown(self):
        token = (await self.get())["token"]
        with patch("spotify_duck_http.LEASE_SECONDS",.12):
            r = await self.duck("start",token,{"depthDb":9})
            handle = (await r.json())["duckToken"]
            await asyncio.sleep(.07)
            await self.duck("level",handle,{"rms":.2,"depthDb":9})
            await asyncio.sleep(.07)
            status = await self.client.get("/duck/status",headers=self.headers)
            self.assertTrue((await status.json())["active"])
            await asyncio.sleep(.1)
            self.assertFalse(self.ducker.started)
            self.assertEqual(self.ducker.stops,1)
            self.assertEqual((await self.duck("stop",handle,{})).status,403)
        self.assertEqual((await self.duck("start",token,{"depthDb":7})).status,200)
        await self.client.close()
        self.assertFalse(self.ducker.started)
        self.assertEqual(self.ducker.stops,2)

    async def test_duck_payload_selection_and_busy(self):
        token = (await self.get())["token"]
        for wrong in (0,25,-1,True,"12",float("nan"),float("inf")):
            self.assertEqual((await self.duck("start",token,{"depthDb":wrong})).status,400)
        self.assertEqual((await self.duck("start",token,{"depthDb":12},Cookie="fixture=test")).status,403)
        self.ducker.fail = RuntimeError()
        self.assertEqual((await self.duck("start",token,{"depthDb":12})).status,409)
        self.ducker.fail = None
        good = await self.duck("start",token,{"depthDb":12})
        handle = (await good.json())["duckToken"]
        for b in ({"rms":1.1,"depthDb":12},{"rms":False,"depthDb":12},{"rms":.1,"depthDb":float("nan")},
                  {"rms":.1,"depthDb":12,"audio":[0,0,0]},{}):
            self.assertEqual((await self.duck("level",handle,b)).status,400)
        oversized = await self.duck("level",handle,{"pcm":"A"*350})
        self.assertEqual(oversized.status,413)
        self.assertFalse(self.ducker.feeds)
        await self.duck("stop",handle,{})
        self.sessions.items=[self.browser]
        self.assertEqual((await self.duck("start",token,{"depthDb":12})).status,409)
        self.assertFalse(self.ducker.started)

    async def test_unavailable_ducker_is_honest(self):
        client = TestClient(TestServer(mod.create_app(self.sessions,inventory=lambda: self.installed,duck_factory=lambda: None)))
        async with client:
            reply = await client.get("/duck/status",headers=self.headers)
            self.assertFalse((await reply.json())["available"])
            spotify = await client.get("/spotify/status",headers=self.headers)
            token = (await spotify.json())["token"]
            r = await client.post("/duck/start",headers={**self.headers,"X-Lum-Spotify-Token":token},json={"depthDb":12})
            self.assertEqual(r.status,503)

    async def changes(self, since=None):
        res = await self.client.get("/spotify/changes", params={"since":since} if since else {}, headers=self.headers)
        self.assertEqual(res.status, 200)
        return await res.json()

    async def test_native_change_broadcast_and_catchup(self):
        initial = await self.changes()
        self.assertTrue(initial["events"])
        self.assertFalse(self.browser.media_handlers or self.browser.playback_handlers)
        revision = initial["revision"]
        watchers = [asyncio.create_task(self.changes(revision)) for _ in range(2)]
        await asyncio.sleep(.015)
        started = time.perf_counter()
        await asyncio.to_thread(self.spotify.emit, self.spotify.media_handlers)
        await asyncio.to_thread(self.spotify.emit, self.spotify.playback_handlers)
        result, other = await asyncio.wait_for(asyncio.gather(*watchers), 1)
        elapsed = time.perf_counter()-started
        print(f"Fixture native callback to both HTTP clients: {elapsed*1000:.1f} ms")
        self.assertEqual(result,other)
        self.assertEqual(int(result["revision"].split(".")[1]),int(revision.split(".")[1])+1)
        self.assertLess(elapsed,.5)
        # A window performing status work while the callback arrived catches up immediately.
        self.assertEqual(await asyncio.wait_for(self.changes(revision),.25), result)
        self.assertEqual(await self.changes("retired-helper.15"), result)
        with patch.object(mod, "CHANGE_WAIT", .03):
            self.assertEqual(await self.changes(result["revision"]), result)
        self.assertEqual(self.spotify.metadata_reads,0)

    async def test_change_subscription_lifetime_and_origin(self):
        wrong = await self.client.get("/spotify/changes",headers={**self.headers,"Origin":"https://example.org"})
        self.assertEqual(wrong.status,403)
        self.assertIsNone(self.sessions.manager_listener)
        self.assertEqual((await self.client.post("/spotify/changes",headers=self.headers)).status,405)
        initial = await self.changes()
        store = Session(mod.STORE_FAMILY + "!Spotify")
        self.sessions.items = [store,self.browser]
        await asyncio.to_thread(self.sessions.manager_listener,None,None)
        moved = await asyncio.wait_for(self.changes(initial["revision"]),1)
        self.assertNotEqual(moved,initial)
        self.assertFalse(self.spotify.media_handlers or self.spotify.playback_handlers)
        self.assertTrue(store.media_handlers and store.playback_handlers)
        await self.client.close()
        self.assertIsNone(self.sessions.manager_listener)
        self.assertFalse(store.media_handlers or store.playback_handlers)

    async def test_event_straddling_heartbeat_is_retained(self):
        first = await self.changes()
        revision = first["revision"]
        # The native notice is already queued, but its debounce expires after this heartbeat.
        with patch.object(mod, "CHANGE_WAIT", .015):
            self.spotify.emit(self.spotify.media_handlers)
            heartbeat = await self.changes(revision)
        self.assertEqual(heartbeat, first)
        updated = await asyncio.wait_for(self.changes(heartbeat["revision"]), .5)
        self.assertNotEqual(updated["revision"], revision)
        self.assertEqual(self.spotify.metadata_reads, 0)

    async def test_unsupported_events_fall_back_and_retry(self):
        self.sessions.failure = NotImplementedError()
        feed = mod.ChangeFeed(self.sessions)
        unavailable = await feed.changes(None)
        self.assertFalse(unavailable["events"])
        self.assertEqual(await asyncio.wait_for(feed.changes(unavailable["revision"]),.25),unavailable)
        self.sessions.failure = None
        feed.retry_after = 0
        recovered = await feed.changes(unavailable["revision"])
        self.assertTrue(recovered["events"])
        self.assertNotEqual(unavailable["revision"],recovered["revision"])
        await feed.close(None)

    async def test_explicit_actions_and_idempotence(self):
        info=await self.get()
        self.assertEqual((info["title"],info["canPlay"],info["canPause"]),("Fixture track",True,False))
        token=info["token"]
        self.assertFalse(self.spotify.calls)
        for action in ("play","play","pause","pause"):
            res=await self.post(token,action)
            self.assertEqual(res.status,200)
            self.assertEqual((await res.json())["playing"],action=="play")
        self.assertEqual(self.spotify.calls,["play","pause"])

    async def test_registered_editions_and_unknown(self):
        self.sessions.items = []
        for known, installed in ((True, ["desktop"]), (True, ["store"]), (True, ["desktop", "store"]),
                                 (True, []), (False, []), (False, ["desktop"])):
            self.installed = (installed, known)
            player = mod.Player(self.sessions, lambda: self.installed)
            state = await player.snapshot(self.headers["Origin"])
            self.assertEqual((state["state"], state["edition"], state["installations"], state["installationKnown"]),
                             ("no_session", "unknown", installed, known))
        self.sessions.failure = ImportError()
        self.installed = (["store"], True)
        info = await self.get()
        self.assertEqual((info["state"], info["installations"], info["installationKnown"], info["edition"]),
                         ("unsupported", ["store"], True, "unknown"))

    async def test_running_player_survives_broker_failure_and_recovers(self):
        self.sessions.failure = OSError("Media service unavailable")
        self.sessions.running_player = lambda: {"state": "detected", "edition": "desktop", "title": "Fixture"}
        offline = await self.get()
        self.assertEqual(("detected", "Fixture"), (offline["state"], offline["title"]))
        self.assertFalse(offline["canPause"] or offline["token"])
        self.sessions.failure = None
        self.assertEqual((await self.get())["state"], "ready")

    async def test_window_transport_is_bound_to_player_and_origin(self):
        from spotify_window_transport import dispatch
        pause, previous, next_track = Mock(), Mock(), Mock()
        controls = {"pause": pause, "previous": previous, "next": next_track}
        window = mod.WindowPlayer("desktop", (123, 456, 7.8))
        self.sessions.failure = OSError("Media service unavailable")
        self.sessions.running_player = lambda: dict(state="ready", edition="desktop", window=window,
                                                     **dispatch(controls, None))
        snap = await self.get()
        self.assertTrue(snap["canPause"] and snap["canNext"])
        self.assertNotIn("window", snap)
        with patch.object(mod, "window_transport", side_effect=lambda p, a: dispatch(controls, a)) as invoked:
            denied = await self.post(snap["token"], "next", Origin="http://localhost:41835")
            self.assertEqual(denied.status, 403)
            invoked.assert_not_called()
            self.assertEqual((await self.post(snap["token"], "play")).status, 200)
            pause.Invoke.assert_not_called()
            self.assertEqual((await self.post(snap["token"], "pause")).status, 200)
            pause.Invoke.assert_called_once()
            self.assertEqual((await self.post(snap["token"], "next")).status, 200)
            next_track.Invoke.assert_called_once()
            self.assertEqual((await self.post(snap["token"], "seekBack15")).status, 409)
            invoked.assert_called_with(window, "seekBack15")
        with patch.object(mod, "window_transport", side_effect=mod.VolumeError("selection_changed")):
            self.assertEqual((await self.post(snap["token"], "previous")).status, 409)
            previous.Invoke.assert_not_called()

    async def test_window_only_cover_switches_and_resends_to_a_fresh_client(self):
        self.sessions.items.remove(self.spotify)
        window = dict(state="ready", edition="desktop", itemUrl=TRACK, title="First track", artist="Artist")
        self.sessions.running_player = lambda: window
        with patch.object(spotify_artwork, "fetch_cover", side_effect=[cover_png("red"), cover_png("blue")]) as fetch:
            original = await self.get()
            identifier = original["artworkId"]
            self.assertRegex(identifier, r"^[0-9a-f]{24}$")
            self.assertEqual((original["itemUrl"], original["title"]), (TRACK, "First track"))
            self.assertTrue(original["artwork"].startswith("data:image/jpeg;base64,"))
            repeated = await self.get(identifier)
            self.assertEqual(repeated["artworkId"], identifier)
            self.assertNotIn("artwork", repeated)
            self.assertEqual((await self.get())["artwork"], original["artwork"])

            window.update(itemUrl=ALBUM, title="Second track")
            changed = await self.get(identifier)
            self.assertEqual((changed["itemUrl"], changed["title"]), (ALBUM, "Second track"))
            self.assertNotEqual(changed["artworkId"], identifier)
            self.assertNotEqual(changed["artwork"], original["artwork"])

            window["state"] = "detected"
            detected = await self.get(changed["artworkId"])
            self.assertEqual(detected["state"], "detected")
            self.assertIsNone(detected["token"])
            self.assertFalse(detected["canPlay"] or detected["canPause"])
            self.assertNotIn("artwork", detected)
            self.assertEqual((await self.get())["artwork"], changed["artwork"])

            del window["itemUrl"]
            missing = await self.get(changed["artworkId"])
            self.assertIsNone(missing["artworkId"])
            self.assertIsNone(missing["artwork"])
            self.assertEqual(fetch.await_args_list, [call(TRACK), call(ALBUM)])

    async def test_native_cover_wins_and_window_fallback_requires_matching_media(self):
        window = dict(state="ready", edition="desktop", itemUrl=TRACK, title="fixture track", artist="fixture artist")
        self.sessions.running_player = Mock(return_value=window)
        self.spotify.thumbnail = object()
        with patch.object(mod, "thumbnail_bytes", new_callable=AsyncMock, return_value=cover_png("red")), \
             patch.object(spotify_artwork, "fetch_cover", new_callable=AsyncMock, return_value=cover_png("blue")) as fetch:
            native = await self.get()
            self.assertTrue(native["artwork"].startswith("data:image/jpeg;base64,"))
            fetch.assert_not_awaited()

            self.spotify.thumbnail = None
            for mismatch in (dict(edition="store"), dict(title="Other track"), dict(artist="Other artist")):
                with self.subTest(mismatch=mismatch):
                    self.sessions.running_player.return_value = dict(window, **mismatch)
                    rejected = await self.get(native["artworkId"])
                    self.assertEqual(rejected["title"], "Fixture track")
                    self.assertIsNone(rejected["artworkId"])
                    self.assertIsNone(rejected["artwork"])
            fetch.assert_not_awaited()

            self.sessions.running_player.return_value = window
            recovered = await self.get(native["artworkId"])
            self.assertEqual(recovered["itemUrl"], TRACK)
            self.assertNotEqual(recovered["artworkId"], native["artworkId"])
            self.assertNotEqual(recovered["artwork"], native["artwork"])
            fetch.assert_awaited_once_with(TRACK)

    async def test_inventory_independent_failures(self):
        for desktop, store, expected in [(False, False, ([],True)), (True, True, (["desktop","store"],True)),
                                         (True, PermissionError, (["desktop"],False)),
                                         (ImportError, True, (["store"],False))]:
            def probe(value):
                if isinstance(value, type):
                    raise value
                return value
            with patch.object(mod, "website_installed", side_effect=lambda: probe(desktop)), \
                 patch.object(mod, "store_installed", side_effect=lambda: probe(store)):
                self.assertEqual(mod.read_installations(),expected)

    async def test_sole_playing_store_then_website(self):
        store = Session(mod.STORE_FAMILY + "!Spotify")
        self.sessions.items.append(store)
        self.installed = (["desktop", "store"], True)
        self.browser.playing = True
        self.assertEqual((await self.get())["state"], "ambiguous")
        store.playing = True
        picked = await self.get()
        self.assertEqual((picked["edition"], picked["installations"]), ("store", ["desktop", "store"]))
        self.spotify.playing = True
        self.assertEqual((await self.get())["state"], "ambiguous")
        store.playing = False
        self.assertEqual((await self.get())["edition"], "desktop")
        self.assertEqual((await self.post(picked["token"], "pause")).status, 409)
        self.assertFalse(store.calls or self.spotify.calls)
        store.get_playback_info = lambda: (_ for _ in ()).throw(OSError())
        self.assertEqual((await self.get())["state"], "ambiguous")

    async def test_cover_pixels_and_polling(self):
        def png(color):
            out = BytesIO()
            with Image.new("RGB", (1200, 800), color) as im:
                im.save(out, format="PNG")
            return out.getvalue()
        self.spotify.thumbnail = object()
        red, blue = png("red"), png("blue")
        with patch.object(mod,"thumbnail_bytes",new_callable=AsyncMock) as reader, \
             patch.object(mod,"encode_artwork",wraps=mod.encode_artwork) as encoder:
            reader.return_value = red
            first = await self.get()
            identifier = first["artworkId"]
            self.assertRegex(identifier, r"^[0-9a-f]{24}$")
            url = first["artwork"]
            with Image.open(BytesIO(base64.b64decode(url.split(",",1)[1]))) as img:
                self.assertEqual((img.format,img.size),("JPEG",(768,512)))
                self.assertGreater(img.getpixel((0,0))[0],245)
            repeated = await self.get(identifier)
            self.assertEqual(repeated["artworkId"], identifier)
            self.assertNotIn("artwork", repeated)
            self.assertEqual(encoder.call_count, 1)
            reader.return_value = blue
            changed = await self.get(identifier)
            self.assertNotEqual(changed["artworkId"], identifier)
            self.assertNotEqual(changed["artwork"], url)
            reader.return_value = b"not an image"
            invalid = await self.get(changed["artworkId"])
            self.assertIsNone(invalid["artworkId"])
            self.assertIsNone(invalid["artwork"])
            reader.side_effect = TimeoutError()
            unavailable = await self.get(identifier)
            self.assertEqual((unavailable["state"], unavailable["artwork"]), ("ready", None))
            self.spotify.thumbnail = None
            absent = await self.get(identifier)
            self.assertEqual((absent["artwork"], absent["artworkId"]), (None,None))

    @unittest.skipUnless(os.name=="nt","Windows apartment regression")
    async def test_fresh_clients_always_receive_cover_from_one_mta(self):
        main_thread=threading.get_ident()
        calls=[]
        def apartment(step):
            kind,qualifier=ctypes.c_int(),ctypes.c_int()
            result=ctypes.windll.ole32.CoGetApartmentType(ctypes.byref(kind),ctypes.byref(qualifier))
            calls.append((step,threading.get_ident(),result,kind.value))
        async def properties():
            apartment("properties")
            return NS(title="Test",artist="Artist",thumbnail=object())
        async def thumbnail(reference):
            apartment("thumbnail")
            buf=BytesIO()
            Image.new("RGB",(30,30),"blue").save(buf,"PNG")
            return buf.getvalue()
        with patch.object(self.spotify,"try_get_media_properties_async",properties), patch.object(mod,"thumbnail_bytes",thumbnail):
            first=await self.get()
            cached=await self.get(first["artworkId"])
            fresh=await self.get()
        self.assertTrue(first["artwork"])
        self.assertEqual(first["artwork"],fresh["artwork"])
        self.assertNotIn("artwork",cached)
        self.assertEqual([c[0] for c in calls],["properties","thumbnail"]*3)
        self.assertTrue(all(thread!=main_thread and code==0 and kind==1 for _,thread,code,kind in calls),calls)
        self.assertEqual(len({c[1] for c in calls}),1)

    async def test_origin_host_and_csrf(self):
        t=(await self.get())["token"]
        for hdr in ({"Origin":"https://example.org"},{"Origin":"null"},{"Host":"attacker.local:41838"}):
            res=await self.post(t,"play",**hdr)
            self.assertEqual(res.status,403)
        self.assertEqual((await self.client.get("/spotify/status")).status,403)
        self.assertEqual((await self.post(t,"play",Origin="http://localhost:41835")).status,403)
        self.assertEqual((await self.post("a"*43,"play")).status,403)
        self.assertEqual((await self.post(t,"volume")).status,400)
        self.assertEqual((await self.client.get("/spotify/control?action=play",headers=self.headers)).status,405)
        opt=await self.client.options("/spotify/control",headers={**self.headers,"Access-Control-Request-Method":"POST"})
        self.assertEqual(opt.status,204)
        self.assertEqual(opt.headers["Access-Control-Allow-Origin"],self.headers["Origin"])
        self.assertFalse(self.spotify.calls)

    async def test_absent_ambiguous_and_platform_unavailable(self):
        t=(await self.get())["token"]
        self.sessions.items.extend([Session("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify")])
        self.assertEqual((await self.get())["state"],"ambiguous")
        self.assertEqual((await self.post(t,"play")).status,409)
        self.sessions.items=[self.browser,Session("not-Spotify.exe")]
        self.assertEqual((await self.get())["state"],"no_session")
        self.sessions.failure=ImportError()
        self.assertEqual((await self.get())["state"],"unsupported")
        self.sessions.failure=OSError("fixture")
        self.assertEqual((await self.get())["state"],"unavailable")
        self.assertFalse(self.spotify.calls)

    async def test_disabled_and_declined(self):
        t=(await self.get())["token"]
        self.spotify.enabled=False
        self.assertEqual((await self.post(t,"play")).status,409)
        self.assertFalse(self.spotify.calls)
        self.spotify.enabled=True
        self.spotify.accept=False
        res=await self.post(t,"play")
        self.assertEqual(res.status,409)
        self.assertEqual((await res.json())["error"],"player_declined")
        self.assertEqual(self.spotify.calls,["play"])

    async def test_transport_flags_commands_and_seek_bounds(self):
        s = self.spotify
        s.transport = dict.fromkeys(s.transport, True)
        td = lambda seconds: timedelta(seconds=seconds)
        s.timeline = NS(start_time=td(0), end_time=td(60), min_seek_time=td(5), max_seek_time=td(55),
                        position=td(8), last_updated_time=datetime.now(timezone.utc))
        current = await self.get()
        token = current["token"]
        self.assertTrue(all(current[k] for k in ("canPrevious","canNext","canStop","canSeek")))
        self.assertEqual(current["timeline"]["position"],8)
        for action in ("previous","next","stop","seekBack15","seekForward15"):
            res = await self.post(token,action)
            self.assertEqual(res.status,200)
            refreshed = await res.json()
            self.assertEqual(refreshed["state"],"ready")
        self.assertEqual(s.calls,["previous","next","stop",("seek",50_000_000),("seek",200_000_000)])
        self.assertEqual(refreshed["timeline"]["position"],20)
        s.timeline.position=td(52.5)
        self.assertEqual((await self.post(token,"seekForward15")).status,200)
        self.assertEqual(s.calls[-1],("seek",550_000_000))
        s.transport.update(dict.fromkeys(s.transport,False))
        before=s.calls[:]
        for action,field in (("previous","canPrevious"),("next","canNext"),("stop","canStop"),("seekForward15","canSeek")):
            res=await self.post(token,action)
            self.assertEqual(res.status,409)
            body=await res.json()
            self.assertFalse(body[field])
            self.assertEqual(body["error"],"control_disabled")
        self.assertEqual(s.calls,before)
        s.transport["is_playback_position_enabled"]=True
        s.timeline.max_seek_time=s.timeline.min_seek_time
        self.assertFalse((await self.get())["canSeek"])
        self.assertEqual((await self.post(token,"seekBack15")).status,409)
        s.timeline=None
        self.assertIsNone((await self.get())["timeline"])
        self.assertEqual((await self.post(token,"seekForward15")).status,409)
        self.assertEqual((await self.post(token,"speed")).status,400)

    async def test_elapsed_position_and_selected_session_only(self):
        s=self.spotify
        stamp=datetime(2026,11,24,tzinfo=timezone.utc)
        s.playing=True
        s.transport["is_playback_position_enabled"]=True
        s.timeline=NS(start_time=timedelta(0),end_time=timedelta(seconds=120),
                      min_seek_time=timedelta(0),max_seek_time=timedelta(seconds=120),
                      position=timedelta(seconds=35),last_updated_time=stamp)
        s.rate=1.5
        with patch.object(mod.time,"time",return_value=stamp.timestamp()+10):
            state=await self.get()
            self.assertEqual(state["timeline"]["position"],50)
            self.assertEqual((await self.post(state["token"],"seekBack15")).status,200)
            self.assertEqual(s.calls,[("seek",350_000_000)])
        self.sessions.items=[self.browser]
        self.assertEqual((await self.post(state["token"],"next")).status,409)
        self.assertFalse(self.browser.calls)

    async def test_replacement_does_not_receive_old_token(self):
        token=(await self.get())["token"]
        newer=Session("Spotify.exe")
        self.sessions.items=[newer]
        self.spotify.enabled=False
        self.assertEqual((await self.post(token,"play")).status,409)
        self.assertFalse(newer.calls)


if __name__=="__main__":
    unittest.main()

