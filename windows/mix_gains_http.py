"""Persistent output-strip settings for the host-owned audio mixer."""
import asyncio
import json
import os
from pathlib import Path

from aiohttp import web
from native_mix_control import ControlError, ControlPlane
import profile_save


DEFAULTS = {"micGain": 100, "musicGain": 100, "micMuted": False, "musicMuted": False}


def unique_fields(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate property")
        result[key] = value
    return result


def load_json(text):
    return json.loads(text, object_pairs_hook=unique_fields)


class MixOutput:
    """Share one Rust state and cable mixer between the explicit-start audio routes."""
    def __init__(self, path=None, mixer=None):
        self.path = (Path(path) if path is not None else
                     Path(os.environ.get("LOCALAPPDATA", Path.home())) / "LumMix" / "output-gains.json")
        self.mixer = mixer
        if self.mixer is None:
            try:
                from cable_mixer import CableMixer
                self.mixer = CableMixer()
            except (ImportError, OSError):
                pass
        self.control = ControlPlane()
        self.lock = asyncio.Lock()
        profile, bad = None, False
        try:
            profile = profile_save.load(self.path, lambda data: self.control.validate_profile("gain", data))
        except FileNotFoundError:
            pass
        except (KeyError, ValueError, OSError):
            bad = True
        state = self.control.call("gain_load", profile=profile, bad=bad)
        if not state["readError"]:
            self.apply(state["gains"])

    @property
    def gains(self):
        return self.control.call("gain_get")["gains"]

    @property
    def revision(self):
        return self.control.call("gain_get")["revision"]

    @property
    def read_error(self):
        return self.control.call("gain_get")["readError"]

    def apply(self, gains):
        if self.mixer is not None:
            self.mixer.set_controls(mic_gain=gains["micGain"] / 100, file_gain=gains["musicGain"] / 100,
                                    mute_mic=gains["micMuted"], mute_file=gains["musicMuted"])

    def persist(self, gains, revision):
        profile_save.save(self.path, {"revision": revision, "gains": gains})

    def snapshot(self, token):
        stream = self.mixer.stream if self.mixer is not None else None
        try:
            active = stream is not None and bool(stream.active)
        except (OSError, RuntimeError):
            active = False
        state = self.control.call("gain_get")
        return dict(state["gains"], routeActive=active, engineAvailable=self.mixer is not None,
                    revision=state["revision"], token=token)

    async def commit(self, proposed):
        await asyncio.to_thread(self.persist, proposed["gains"], proposed["revision"])
        self.apply(proposed["gains"])
        self.control.call("gain_commit", profile=proposed)

    def token_for(self, origin):
        return self.control.call("token", origin=origin)

    def authorized_token(self, origin, supplied, body=None):
        return self.control.call("authorize", origin=origin, provided=list(supplied), body=body)

    async def handle(self, request):
        if request.headers.getall("Cookie", []):
            return web.json_response({"error": "cookie_free_request_required"}, status=403)
        if self.read_error:
            return web.json_response({"error": "saved_settings_unreadable"}, status=503)
        origin = request.headers["Origin"]
        if request.method == "GET":
            token = self.token_for(origin)
            async with self.lock:
                return web.json_response(self.snapshot(token))
        token = self.authorized_token(origin, request.headers.getall("X-Lum-Mix-Token", []))
        if token is None:
            return web.json_response({"error": "refresh_mix_settings"}, status=403)
        if request.content_type != "application/json":
            return web.Response(status=415)
        try:
            patch = await request.json(loads=load_json)
            if not self.control.call("validate_gain", patch=patch):
                raise ValueError()
        except (ValueError, UnicodeError):
            return web.json_response({"error": "invalid_settings"}, status=400)
        async with self.lock:
            try:
                proposed = self.control.call("gain_plan", patch=patch)
                if proposed is not None:
                    task = asyncio.create_task(self.commit(proposed))
                    try:
                        await asyncio.shield(task)
                    except asyncio.CancelledError:
                        await task
                        raise
            except (OSError, ControlError):
                return web.json_response(dict(self.snapshot(token), error="settings_not_saved"), status=503)
            return web.json_response(self.snapshot(token))

    def install(self, app):
        app[MIX_OUTPUT] = self
        app.router.add_get("/mix/gains", self.handle, allow_head=False)
        app.router.add_post("/mix/gains", self.handle)


MIX_OUTPUT = web.AppKey("lum_mix_output", MixOutput)
