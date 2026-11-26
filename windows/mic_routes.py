"""Explicit, stable Windows microphone selection and one shared native cable route."""
import asyncio
import time

from aiohttp import web
from mix_gains_http import load_json
from native_audio import device_pair, endpoint_inventory
from native_mix_control import ControlPlane
import profile_save


class RouteError(Exception):
    def __init__(self, code):
        super().__init__(code)
        self.code = code


def signed_cables():
    from cable_setup import qualified_pair, restart_pending
    obj = qualified_pair()
    if restart_pending(obj.get("boot")):
        return {"error": "cable_restart_needed"}
    if obj.get("signed") and obj.get("state") == "ready":
        from cable_alias import labels
        ids = obj.get("endpointIds", {})
        return dict(ids, **labels(ids))
    return {}


def connected_headsets():
    from detect_devices import get_scan
    return " ".join(get_scan().get("connected", [])).lower()


def recommended(rows, saved, connected):
    control = ControlPlane()
    control.call("mic_load", profile={"id": saved}, bad=False)
    return control.call("mic_state", rows=rows, connected=connected)["recommendedId"]


def native_indexes(input_id, cable_ids):
    """Bind both ends by IMMDevice ID from PortAudio, even if display names collide."""
    sinks = cable_ids.get("Render")
    if not sinks or not cable_ids.get("Capture"):
        raise RouteError("cable_unavailable")
    mic, sink = device_pair(input_id, sinks)
    if mic < 0 or sink < 0:
        raise RouteError("restart_audio_helper")
    return mic, sink


def write_selection(path, selected):
    profile_save.save(path, {"schema": 1, "id": selected})


class MicRoutes:
    def __init__(self, output, *, inventory=endpoint_inventory, cables=signed_cables, headsets=connected_headsets,
                 indexes=native_indexes, alias=None):
        from cable_alias import ensure
        self.alias = alias or ensure
        self.alias_error = None
        self.output, self.control = output, output.control
        self.inventory, self.cables, self.headsets, self.indexes = inventory, cables, headsets, indexes
        self.path = output.path.with_name("microphone-input.json")
        self.cable_ids, self.connected = {}, ""
        self.cache_until = 0
        self.watch = None
        profile, bad = None, False
        try:
            profile = profile_save.load(self.path, lambda data: self.control.validate_profile("mic", data))
        except FileNotFoundError:
            pass
        except (OSError, ValueError, KeyError, AttributeError, TypeError):
            bad = True
        self.control.call("mic_load", profile=profile, bad=bad)

    @property
    def active(self):
        return self.control.call("mic_active")

    @property
    def saved(self):
        return self.control.call("mic_state")["savedId"]

    @property
    def cable_ready(self):
        return bool(self.cable_ids.get("Render") and self.cable_ids.get("Capture"))

    async def devices(self, force=False):
        rows = await asyncio.to_thread(self.inventory)
        if force or time.monotonic() >= self.cache_until:
            self.cable_ids, self.connected = await asyncio.gather(
                asyncio.to_thread(self.cables), asyncio.to_thread(self.headsets))
            self.cache_until = time.monotonic() + 30
        return rows

    def snapshot(self, rows, token, error=None):
        selected = self.control.call("mic_state", rows=rows, connected=self.connected,
                                     cableError=self.cable_ids.get("error"))
        mixer = self.output.mixer
        stream = mixer.stream if mixer is not None else None
        try:
            running = bool(selected["activeId"] and stream is not None and stream.active)
            latency = stream.latency if running else None
        except (OSError, RuntimeError):
            running, latency = False, None
        source = getattr(mixer, "spotify_feed", None) if running else None
        return dict(selected, spotifyCapture=source.state if source else "off",
                    spotifyEdition=source.edition if source else None,
                    captureName=self.cable_ids.get("captureName"),
                    captureAlias=self.cable_ids.get("captureAlias", "unavailable"),
                    aliasError=self.alias_error, inputs=rows, activeId=selected["activeId"] if running else None,
                    routeActive=running, cableReady=self.cable_ready, error=error or selected["error"],
                    token=token, latencyMode=mixer.latency_mode if running else None,
                    latencyMs=dict(input=round(latency[0] * 1000, 1), output=round(latency[1] * 1000, 1)) if latency else None,
                    micRms=min(1, max(0, float(mixer.mic_rms))) if running else None)

    async def stop(self, error=None):
        self.control.call("mic_stopped", error=error)
        if self.output.mixer is not None and self.output.mixer.stream is not None:
            await asyncio.to_thread(self.output.mixer.stop)

    async def monitor(self):
        while self.active:
            await asyncio.sleep(2)
            async with self.output.lock:
                if not self.active:
                    return
                try:
                    live = await asyncio.to_thread(self.inventory)
                    stream = self.output.mixer.stream
                    error = self.control.call("mic_interrupted", rows=live, running=bool(stream and stream.active))
                    if error:
                        await self.stop(error)
                except Exception:
                    await self.stop("route_interrupted")

    async def select(self, selected):
        await asyncio.to_thread(write_selection, self.path, selected)
        self.control.call("mic_selected", id=selected)

    async def start(self, plan, origin, token):
        rename = await asyncio.to_thread(self.alias, self.cable_ids)
        self.cable_ids.update(captureName=rename["captureName"], captureAlias=rename["captureAlias"])
        self.alias_error = rename["aliasError"]
        channels = await asyncio.to_thread(self.indexes, plan["id"], self.cable_ids)
        try:
            await asyncio.to_thread(self.output.mixer.start_mic_route, *channels,
                                    signed_render_ids=self.cable_ids["Render"])
        except Exception:
            raise RouteError("route_start_failed") from None
        owner_token = plan["ownerToken"]
        self.control.call("mic_started", id=plan["id"], origin=origin, token=token, ownerToken=owner_token)
        if self.watch is None or self.watch.done():
            self.watch = asyncio.create_task(self.monitor())
        return {"ownerToken": owner_token}

    async def perform(self, body, origin, token, rows):
        mixer = self.output.mixer
        plan = self.control.call("mic_plan", body=body, origin=origin, rows=rows, connected=self.connected,
                                 routeAvailable=mixer is not None and mixer.stream is None,
                                 cableReady=self.cable_ready, cableError=self.cable_ids.get("error"))
        if not plan:
            return {}
        if "routeError" in plan:
            raise RouteError(plan["routeError"])
        action = plan["action"]
        if action == "stop":
            operation = self.stop()
        elif action == "select":
            operation = self.select(plan["id"])
        else:
            operation = self.start(plan, origin, token)
        task = asyncio.create_task(operation)
        try:
            return (await asyncio.shield(task)) or {}
        except asyncio.CancelledError:
            await task
            raise

    async def handle(self, req):
        if req.headers.getall("Cookie", []):
            return web.json_response({"error": "cookie_free_request_required"}, status=403)
        origin = req.headers["Origin"]
        body = None
        if req.method == "POST":
            if req.content_type != "application/json":
                return web.json_response({"error": "expected_json"}, status=415)
            try:
                body = await req.json(loads=load_json)
                if not self.control.call("validate_mic", body=body):
                    raise ValueError()
            except (KeyError, TypeError, ValueError):
                return web.json_response({"error": "invalid_settings"}, status=400)
        async with self.output.lock:
            token = self.output.token_for(origin)
            if body and self.output.authorized_token(origin, req.headers.getall("X-Lum-Mix-Token", []), body) is None:
                return web.json_response({"error": "refresh_mix_settings"}, status=403)
            rows, extra = [], {}
            action = body["action"] if body else None
            try:
                if action in ("release", "stop"):
                    await self.perform(body, origin, token, rows)
                rows = await self.devices(force=action in ("start", "setup"))
                if action and action not in ("release", "stop"):
                    extra = await self.perform(body, origin, token, rows)
                return web.json_response(self.snapshot(rows, token) | extra)
            except RouteError as exc:
                return web.json_response(self.snapshot(rows, token, exc.code), status=409)
            except Exception:
                error = "settings_not_saved" if action == "select" else "route_unavailable"
                return web.json_response(self.snapshot(rows, token, error), status=503)

    async def shutdown(self, app):
        async with self.output.lock:
            if self.active:
                await self.stop()
        if self.watch:
            self.watch.cancel()
            await asyncio.gather(self.watch, return_exceptions=True)

    def install(self, app):
        app.router.add_get("/mix/mic", self.handle, allow_head=False)
        app.router.add_post("/mix/mic", self.handle)
        app.on_shutdown.append(self.shutdown)
