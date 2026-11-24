"""Persistent output-strip settings for the host-owned microphone/local-file mixer."""
import asyncio
import json
import math
import os
from pathlib import Path
import secrets
import tempfile
import time

from aiohttp import web


DEFAULTS = {"micGain": 100, "musicGain": 100, "micMuted": False, "musicMuted": False}
TOKEN_TTL = 600


def checked_patch(data):
    if not isinstance(data, dict) or not data or data.keys() - DEFAULTS.keys():
        raise ValueError("Expected a nonempty control patch")
    for field, value in data.items():
        if field.endswith("Gain"):
            if type(value) not in (int, float) or not 0 <= value <= 200 or not math.isfinite(value):
                raise ValueError("Gain must be a percentage from 0 to 200")
        elif type(value) is not bool:
            raise ValueError("Mute must be a boolean")
    return data


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
    """One native CableMixer is shared by gain setters and the explicit-start route host."""
    def __init__(self, path=None, mixer=None):
        self.path = Path(path) if path is not None else Path(os.environ.get("LOCALAPPDATA",Path.home()))/"LumMix"/"output-gains.json"
        self.mixer = mixer
        if self.mixer is None:
            try:
                from cable_mixer import CableMixer
                self.mixer = CableMixer()
            except (ImportError, OSError):
                pass  # Transport and persistent settings are usable without the optional native engine.
        self.gains = DEFAULTS.copy()
        self.revision = 0
        self.read_error = False
        self.tokens = {}
        self.lock = asyncio.Lock()
        try:
            saved = load_json(self.path.read_text(encoding="utf-8"))
            if not isinstance(saved,dict) or type(saved.get("revision")) is not int or saved["revision"] < 0:
                raise ValueError("Invalid saved revision")
            settings = checked_patch(saved["gains"])
            self.gains.update(settings)
            self.revision = saved["revision"]
        except FileNotFoundError:
            pass
        except (KeyError, ValueError, OSError):
            self.read_error = True
        if not self.read_error:
            self.apply(self.gains)

    def apply(self, gains):
        if self.mixer is not None:
            self.mixer.set_controls(mic_gain=gains["micGain"]/100, file_gain=gains["musicGain"]/100,
                                    mute_mic=gains["micMuted"], mute_file=gains["musicMuted"])

    def persist(self, gains, revision):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        name = None
        try:
            with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=self.path.parent, delete=False) as f:
                name = f.name
                json.dump({"revision": revision, "gains": gains}, f, allow_nan=False)
                f.flush()
                os.fsync(f.fileno())
            os.replace(name, self.path)
        finally:
            if name is not None:
                Path(name).unlink(missing_ok=True)

    def snapshot(self, token):
        stream = self.mixer.stream if self.mixer is not None else None
        try:
            active = stream is not None and bool(stream.active)
        except (OSError, RuntimeError):
            active = False
        return dict(self.gains, routeActive=active, engineAvailable=self.mixer is not None,
                    revision=self.revision, token=token)

    async def commit(self, updated):
        revision = self.revision + 1
        await asyncio.to_thread(self.persist, updated, revision)
        self.apply(updated)
        self.gains, self.revision = updated, revision

    def token_for(self, origin):
        token, expires = self.tokens.get(origin, (None, 0))
        if time.monotonic() >= expires:
            token = secrets.token_urlsafe(32)
            self.tokens[origin] = (token, time.monotonic()+TOKEN_TTL)
        return token

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
        provided = request.headers.getall("X-Lum-Mix-Token", [])
        token, expires = self.tokens.get(origin, (None, 0))
        if (len(provided) != 1 or token is None or not provided[0].isascii()
                or time.monotonic() >= expires or not secrets.compare_digest(provided[0],token)):
            return web.json_response({"error": "refresh_mix_settings"}, status=403)
        if request.content_type != "application/json":
            return web.Response(status=415)
        try:
            patch = checked_patch(await request.json(loads=load_json))
        except (ValueError, UnicodeError):
            return web.json_response({"error": "invalid_settings"}, status=400)
        async with self.lock:
            updated = self.gains | patch
            if updated != self.gains:
                task = asyncio.create_task(self.commit(updated))
                try:
                    # Keep ordering and disk/native state coherent even if the sending window closes.
                    await asyncio.shield(task)
                except asyncio.CancelledError:
                    await task
                    raise
                except OSError:
                    return web.json_response(dict(self.snapshot(token), error="settings_not_saved"), status=503)
            return web.json_response(self.snapshot(token))

    def install(self, app):
        app[MIX_OUTPUT] = self
        app.router.add_get("/mix/gains", self.handle, allow_head=False)
        app.router.add_post("/mix/gains", self.handle)


MIX_OUTPUT = web.AppKey("lum_mix_output", MixOutput)
