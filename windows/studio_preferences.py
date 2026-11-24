"""Per-Windows-user app preferences. Never starts an audio device or changes system settings."""
import asyncio
import base64
import hmac
import json
import os
import tempfile
from pathlib import Path

from aiohttp import web

from mix_gains_http import load_json


SWITCHES = {"sounds", "followTheme", "dark", "companion", "globalPulse", "closeToTray"}
JPEG_PREFIX = "data:image/jpeg;base64,"


def checked(settings):
    if not isinstance(settings, dict):
        raise ValueError()
    for key, value in settings.items():
        if key in SWITCHES:
            valid = type(value) is bool
        elif key == "snap":
            valid = type(value) is int and value in (0, 5, 10, 20)
        elif key == "background":
            valid = value in ("album", "custom")
        elif key == "image":
            if not isinstance(value, str) or len(value) >= 400000 or not value.startswith(JPEG_PREFIX):
                raise ValueError()
            data = base64.b64decode(value[len(JPEG_PREFIX):], validate=True)
            valid = data.startswith(b"\xff\xd8\xff") and data.endswith(b"\xff\xd9")
        else:
            raise ValueError()
        if not valid:
            raise ValueError()
    return settings


class StudioPreferences:
    def __init__(self, output):
        self.output = output
        self.path = output.path.with_name("studio-preferences.json")
        self.lock = asyncio.Lock()

    def read(self):
        try:
            return checked(load_json(self.path.read_text("utf-8"))["settings"])
        except FileNotFoundError:
            return {}

    def save(self, settings):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        fd, name = tempfile.mkstemp(dir=self.path.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as stream:
                json.dump({"settings": settings}, stream, separators=(",", ":"))
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(name, self.path)
        finally:
            Path(name).unlink(missing_ok=True)

    async def handle(self, request):
        if request.headers.getall("Cookie", []):
            return web.json_response({"error":"cookie_free_request_required"}, status=403)
        token = self.output.token_for(request.headers["Origin"])
        patch, migrate = {}, False
        if request.method == "POST":
            provided = request.headers.getall("X-Lum-Mix-Token", [])
            if len(provided) != 1 or not provided[0].isascii() or not hmac.compare_digest(provided[0], token):
                return web.json_response({"error":"refresh_settings"}, status=403)
            if request.content_type != "application/json":
                return web.Response(status=415)
            try:
                # This route alone allows one resized JPEG; retain the smaller cap on audio controls.
                data = await request.clone(client_max_size=401000).json(loads=load_json)
                if not isinstance(data, dict) or set(data) - {"settings", "migrate"}:
                    raise ValueError()
                patch = checked(data["settings"])
                migrate = data.get("migrate", False)
                if type(migrate) is not bool or not patch:
                    raise ValueError()
            except (KeyError, ValueError, UnicodeError):
                return web.json_response({"error":"invalid_preferences"}, status=400)
        async with self.lock:
            try:
                # No await between reading and replacing, so cancellation cannot interleave writes.
                existing = self.read()
                settings = patch | existing if migrate else existing | patch
                if settings != existing:
                    self.save(settings)
            except (TypeError, ValueError, OSError, KeyError):
                return web.json_response({"error":"preferences_unavailable"}, status=503)
        return web.json_response({"settings": settings, "token": token})

    def install(self, app):
        app.router.add_get("/mix/preferences", self.handle, allow_head=False)
        app.router.add_post("/mix/preferences", self.handle)
