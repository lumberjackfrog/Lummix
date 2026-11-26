"""Read and set Spotify’s own visible player volume through Windows UI Automation."""
import asyncio
from collections import OrderedDict
import math
import time

from aiohttp import web

from spotify_locations import spotify_processes
from spotify_native import provider as media_provider


class VolumeError(Exception):
    def __init__(self, reason, http=409):
        self.reason = reason
        self.http = http


# Match the accessible slider specifically, never the playback seek/resize sliders.
_VOLUME_LABELS = {"change volume", "cambiar volumen", "cambiar el volumen", "ajustar volumen", "ajustar el volumen"}
_MUTE_LABELS = {"mute": False, "unmute": True, "silenciar": False, "activar sonido": True,
                "activar el sonido": True, "quitar silencio": True, "dejar de silenciar": True}
_REMOTE_PREFIXES = ("listening on ", "playing on ", "escuchando en ", "reproduciendo en ")


def spotify_windows(edition):
    """Only inspect the known installed app's top-level Windows. Do not activate it."""
    try:
        native = media_provider()
        if native is None:
            raise OSError("The Rust media library is required for Spotify window discovery")
        processes = spotify_processes()
        return [(handle, pid, processes[pid][1]) for handle, pid in native.call(13)
                if pid in processes and processes[pid][2] == edition]
    except (OSError, RuntimeError):
        raise VolumeError("window_unavailable", 503) from None


def controls(ui, module, root, kind, scope=None):
    found = root.FindAll(scope if scope is not None else module.TreeScope_Descendants,
                         ui.CreatePropertyCondition(module.UIA_ControlTypePropertyId, kind))
    return [found.GetElement(i) for i in range(found.Length)]


def volume_target(ui, mod, windows):
    choices = []
    for identity in windows:
        root = ui.ElementFromHandle(identity[0])
        for slider in controls(ui, mod, root, mod.UIA_SliderControlTypeId):
            if slider.CurrentName.strip().casefold() not in _VOLUME_LABELS:
                continue
            bar = ui.ControlViewWalker.GetParentElement(slider)
            # Spotify's accessible transport groups the slider and mute button in the
            # now-playing bar. Some Chromium builds wrap the range in a label.
            mute = []
            for _ in range(3):
                if not bar:
                    break
                buttons = controls(ui, mod, bar, mod.UIA_ButtonControlTypeId, mod.TreeScope_Children)
                names = [(b.CurrentName.strip().casefold(), b) for b in buttons]
                if any(name.startswith(_REMOTE_PREFIXES) for name, _ in names):
                    raise VolumeError("connect_device")
                mute = [(name, button) for name, button in names if name in _MUTE_LABELS]
                if len(mute) == 1:
                    break
                bar = ui.ControlViewWalker.GetParentElement(bar)
            else:
                continue
            if not bar or len(mute) != 1:
                continue
            raw = slider.GetCurrentPattern(mod.UIA_RangeValuePatternId)
            if not raw or not slider.CurrentIsEnabled:
                raise VolumeError("slider_unavailable")
            pat = raw.QueryInterface(mod.IUIAutomationRangeValuePattern)
            if pat.CurrentIsReadOnly or (pat.CurrentMinimum, pat.CurrentMaximum) not in ((0, 1), (0, 100)):
                raise VolumeError("slider_unavailable")
            key = (*identity, tuple(slider.GetRuntimeId()))
            choices.append((key, pat, mute[0][1]))
    if len(choices) != 1:
        raise VolumeError("ambiguous_player_slider" if choices else "no_player_slider")
    return choices[0]


def apply_target(target, expected, change, module):
    key, slider, mute = target
    if change is not None and expected != key:
        raise VolumeError("selection_changed" if expected is not None else "read_volume_first")
    def read():
        amount = 100 * slider.CurrentValue / slider.CurrentMaximum
        label = mute.CurrentName.strip().casefold()
        if label not in _MUTE_LABELS or not math.isfinite(amount) or not 0 <= amount <= 100:
            raise VolumeError("slider_unavailable")
        return dict(state="ready", source="spotify_player", volume=round(amount, 2),
                    muted=_MUTE_LABELS[label])

    if change is None:
        return key, read()
    if "volume" in change:
        wanted = change["volume"]
        if abs(read()["volume"] - wanted) > .5:
            slider.SetValue(wanted * slider.CurrentMaximum / 100)
        def applied(state):
            return abs(state["volume"] - wanted) <= .5
    else:
        wanted = change["muted"]
        if read()["muted"] != wanted:
            action = mute.GetCurrentPattern(module.UIA_InvokePatternId)
            if not action or not mute.CurrentIsEnabled:
                raise VolumeError("mute_unavailable")
            action.QueryInterface(module.IUIAutomationInvokePattern).Invoke()
        def applied(state):
            return state["muted"] == wanted
    # Chromium acknowledges the accessible action asynchronously. Return only an
    # actual provider readback; a successfully dispatched call is not a setting.
    deadline = time.monotonic() + .55
    while True:
        state = read()
        if applied(state):
            return key, state
        if time.monotonic() >= deadline:
            raise VolumeError("player_did_not_confirm")
        time.sleep(.025)


def native_volume(edition, expected, change):
    try:
        native = media_provider()
        if native is None:
            raise OSError("The Rust media library is required for Spotify player volume")
        result = native.volume(spotify_windows(edition), expected, change)
        if isinstance(result, dict):
            raise VolumeError(result["reason"], result["http"])
        key, state = result
        return key, state
    except (OSError, RuntimeError):
        raise VolumeError("slider_unavailable", 503) from None


def valid_change(body):
    if not isinstance(body, dict):
        return False
    if set(body) == {"muted"}:
        return isinstance(body["muted"], bool)
    if set(body) == {"volume"} and type(body["volume"]) in (int, float):
        return 0 <= body["volume"] <= 100 and math.isfinite(body["volume"])
    return False


class VolumeRoutes:
    def __init__(self, player, editions, native=native_volume):
        self.player = player
        self.editions = editions
        self.native = native
        self.bindings = OrderedDict()
        self.lock = asyncio.Lock()

    async def handle(self, request):
        def unavailable(code, reason):
            return web.json_response(dict(state="unavailable", error=reason), status=code)

        tokens = request.headers.getall("X-Lum-Spotify-Token", [])
        if len(tokens) != 1 or len(tokens[0]) != 43:
            return unavailable(403, "refresh_selection")
        token = tokens[0]
        change = None
        if request.method == "POST":
            if request.content_type != "application/json":
                return unavailable(415, "expected_json")
            try:
                change = await request.json()
            except (ValueError, UnicodeError):
                return unavailable(400, "invalid_command")
            if not valid_change(change):
                return unavailable(400, "invalid_command")
        async with self.lock:
            code, reason = await self.player.validate_duck_start(request.headers["Origin"], token)
            if code != 200:
                self.bindings.pop(token, None)
                return unavailable(code, reason)
            selection = self.player.tokens.get(token)
            edition = self.editions.get(selection.session.source_app_user_model_id.casefold()) if selection else None
            if not edition:
                return unavailable(409, "selection_changed")
            if change is not None and token not in self.bindings:
                return unavailable(409, "read_volume_first")
            try:
                key, result = await asyncio.to_thread(self.native, edition, self.bindings.get(token), change)
            except VolumeError as error:
                self.bindings.pop(token, None)
                return unavailable(error.http, error.reason)
            except (ImportError, OSError, NotImplementedError):
                return unavailable(503, "slider_unavailable")
            self.bindings[token] = key
            self.bindings.move_to_end(token)
            while len(self.bindings) > 64:
                self.bindings.popitem(last=False)
        return web.json_response(result)

    def install(self, app):
        app.router.add_get("/spotify/volume", self.handle, allow_head=False)
        app.router.add_post("/spotify/volume", self.handle)
