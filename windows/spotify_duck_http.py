"""Authenticated loopback lease for the personal Spotify ducker. Receives RMS only."""
import asyncio
import hmac
from importlib.util import find_spec
import math
import os
import secrets
import time

from aiohttp import web


DUCK_METHODS = {"/duck/status": "GET", "/duck/start": "POST", "/duck/level": "POST", "/duck/stop": "POST"}
LEASE_SECONDS = 2.0


def native_controller():
    if os.name != "nt" or not all(find_spec(m) for m in ("pycaw", "comtypes", "psutil")):
        return None
    try:
        from spotify_duck import PersonalSpotifyDucker
    except (ImportError, OSError):
        return None
    return PersonalSpotifyDucker()


def number(value, minimum, maximum):
    return type(value) in (int, float) and minimum <= value <= maximum and math.isfinite(value)


class DuckRoutes:
    def __init__(self, validate_player, factory=native_controller, route_level=lambda: None):
        # The native constructor is passive. Only start_external creates a COM worker.
        self.controller = factory()
        self.validate_player = validate_player
        self.route_level = route_level
        self.routed = False
        self.guard = asyncio.Lock()
        self.token = self.origin = self.timer = None
        self.deadline = 0
        self.closed = False

    def state(self):
        if self.controller is None:
            return {"available": False, "active": False, "state": "unavailable", "sessions": 0, "reductionDb": 0}
        return dict(self.controller.status(), available=True, active=self.token is not None,
                    micRouteActive=self.route_level() is not None)

    def renew(self):
        self.deadline = time.monotonic() + LEASE_SECONDS
        # One timer per owner. Frequent level samples only move its deadline.
        if self.timer is None:
            self.timer = asyncio.get_running_loop().call_later(LEASE_SECONDS, self.check_deadline)

    def check_deadline(self):
        remaining = self.deadline - time.monotonic()
        if remaining > 0:
            self.timer = asyncio.get_running_loop().call_later(remaining, self.check_deadline)
        else:
            self.timer = None
            asyncio.create_task(self.expire())

    async def expire(self):
        async with self.guard:
            if self.token and time.monotonic() >= self.deadline:
                await self.restore()

    async def restore(self):
        if self.timer:
            self.timer.cancel()
            self.timer = None
        if self.token:
            # Stop waits for the controller's own COM thread to restore its sessions.
            stopped = asyncio.create_task(asyncio.to_thread(self.controller.stop))
            try:
                await asyncio.shield(stopped)
            except asyncio.CancelledError:
                await stopped
                self.token = self.origin = None
                raise
        self.token = self.origin = None
        self.routed = False

    async def close(self, app):
        async with self.guard:
            self.closed = True
            await self.restore()

    def reply(self, code=200, **fields):
        return web.json_response(dict(self.state(), **fields), status=code)

    async def handle(self, request):
        if request.headers.getall("Cookie", []):
            return web.json_response({"error": "cookie_free_request_required"}, status=403)
        if request.method == "GET":
            return self.reply()
        start = request.path == "/duck/start"
        header = "X-Lum-Spotify-Token" if start else "X-Lum-Duck-Token"
        values = request.headers.getall(header, [])
        if len(values) != 1 or len(values[0]) != 43 or not values[0].isascii():
            return web.json_response({"error": "invalid_capability"}, status=403)
        if request.content_type != "application/json":
            return web.Response(status=415)
        try:
            body = await request.json()
        except (ValueError, UnicodeError):
            return web.Response(status=400)
        level = request.path == "/duck/level"
        routed_start = start and isinstance(body, dict) and body.get("source") == "route"
        expected = set()
        if start or level:
            expected.add("depthDb")
        if routed_start:
            expected.add("source")
        if level and not self.routed:
            expected.add("rms")
        if (not isinstance(body, dict) or set(body) != expected
                or (start or level) and not number(body["depthDb"], int(start), 24)
                or level and not self.routed and not number(body["rms"], 0, 1)):
            return web.json_response({"error": "invalid_parameters"}, status=400)
        origin = request.headers["Origin"]
        if start:
            code, reason = await self.validate_player(origin, values[0])
            if code != 200:
                return self.reply(code, error=reason)
        async with self.guard:
            if self.closed or self.controller is None:
                return self.reply(503, error="unavailable")
            if self.token and time.monotonic() >= self.deadline:
                await self.restore()
            if start:
                if self.token:
                    return self.reply(409, error="already_owned")
                if routed_start and self.route_level() is None:
                    return self.reply(409, error="mic_route_stopped")
                started = asyncio.create_task(asyncio.to_thread(self.controller.start_external, body["depthDb"]))
                try:
                    await asyncio.shield(started)
                except asyncio.CancelledError:
                    # A disconnected browser cannot orphan a controller that is still starting.
                    await started
                    await asyncio.to_thread(self.controller.stop)
                    raise
                except RuntimeError:
                    return self.reply(409, error="controller_busy")
                except (ImportError, OSError, ValueError):
                    return self.reply(503, error="unavailable")
                self.routed = routed_start
                self.origin, self.token = origin, secrets.token_urlsafe(32)
                self.renew()
                return self.reply(duckToken=self.token)
            if self.token is None or self.origin != origin or not hmac.compare_digest(self.token, values[0]):
                return self.reply(403, error="invalid_capability")
            if level and self.controller.status()["state"] != "error":
                rms = self.route_level() if self.routed else body["rms"]
                if rms is None:
                    await self.restore()
                    return self.reply(409, error="mic_route_stopped")
                self.controller.set_depth(body["depthDb"])
                self.controller.feed_mic_rms(rms)
                self.renew()
                return self.reply()
            await self.restore()
            if self.controller.status()["state"] == "error":
                return self.reply(503, error="controller_error")
            return self.reply()

    def install(self, app):
        for path, method in DUCK_METHODS.items():
            app.router.add_route(method, path, self.handle)
        app.on_shutdown.append(self.close)
