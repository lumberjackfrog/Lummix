"""Explicit, stable Windows microphone selection and one shared native cable route."""
import asyncio
import ctypes
import json
import secrets
import profile_save
import time
from aiohttp import web
from mix_gains_http import load_json

HW = "{A8B865DD-2E3D-4094-AD97-E593A70C75D6} 8"
INSTANCE = "{B3F8FA53-0004-438E-9003-51A46E139BFC} 2"
FORM = "{1DA5D803-D492-4EDD-8C23-E0C0FFEE7F0E} 0"
BRANDS = ("pico", "oculus", "quest", "vive", "index", "pimax")


class RouteError(Exception):
    def __init__(self, code):
        super().__init__(code)
        self.code = code


def signed_cables():
    from cable_setup import powershell
    result = powershell("cable_setup_status.ps1")
    obj = json.loads(result.stdout) if result.returncode == 0 else {}
    if obj.get("signed") and obj.get("state") == "ready":
        from cable_alias import labels
        ids=obj.get("endpointIds",{})
        return dict(ids,**labels(ids))
    return {}


def capture_kind(name, props):
    full = (name + " " + str(props.get(HW, "")) + " " + str(props.get(INSTANCE, ""))).lower()
    if any(word in full for word in ("vbaudio", "vb-audio", "loopback", "stereo mix", "what u hear", "voicemeeter",
                                      "steam streaming", "streaming speaker", "sonar", "steelseries sonar")):
        return None
    hardware = any(bus in full for bus in ("usb\\", "hdaudio\\", "intelaudio\\", "bth", "acp\\", "amdacp", "acpi\\", "pci\\", "soundwire\\"))
    if hardware:
        return "headset" if props.get(FORM) == 5 or any(x in full for x in ("headset","headphone","hands-free",*BRANDS)) else "physical"
    if any(brand in full for brand in BRANDS) and ("mic" in full or props.get(FORM) in (4, 5)):
        return "headsetBridge"
    return None


def endpoint_inventory():
    import comtypes
    from pycaw.pycaw import AudioUtilities
    comtypes.CoInitialize()
    try:
        en = AudioUtilities.GetDeviceEnumerator()
        def default(role):
            try:
                return en.GetDefaultAudioEndpoint(1, role).GetId().lower()
            except (OSError, comtypes.COMError):
                return None
        std, comm = default(0), default(2)
        rows = []
        for dev in AudioUtilities.GetAllDevices(1, 1):
            k = capture_kind(dev.FriendlyName or "", dev.properties)
            if k:
                ident = dev.id.lower()
                rows.append(dict(id=ident, name=dev.FriendlyName, headset=k!="physical",
                                 kind=k, defaultInput=ident==std, defaultCommunications=ident==comm))
        return sorted(rows, key=lambda x:(x["name"].casefold(),x["id"]))
    finally:
        comtypes.CoUninitialize()


def connected_headsets():
    # Only a currently connected known model is evidence for a persistent streaming driver.
    from detect_devices import get_scan
    return " ".join(get_scan().get("connected", [])).lower()


def recommended(rows, saved, connected):
    if saved and any(r["id"]==saved for r in rows):
        return saved
    pool = []
    for row in rows:
        live_hmd = row["headset"] and any(
            b in row["name"].lower() and b in connected for b in BRANDS)
        if row["kind"]=="headsetBridge" and not live_hmd:
            continue
        pool.append(((live_hmd, row["kind"]=="headset", row["defaultCommunications"],
                      row["defaultInput"]), row["id"]))
    return max(pool, key=lambda pair:pair[0])[1] if pool else None


def native_indexes(input_id, cable_ids):
    """Bind both ends by IMMDevice ID from PortAudio, even if display names collide."""
    import comtypes
    import sounddevice as sd
    from pycaw.api.mmdeviceapi import IMMDevice
    from cable_mixer import _devices, RENDERS
    dll = ctypes.CDLL(sd._libname)
    get_device = dll.PaWasapi_GetIMMDevice
    get_device.argtypes = (ctypes.c_int,ctypes.POINTER(ctypes.c_void_p))
    signed_sinks = {i.lower() for i in cable_ids.get("Render", [])}
    if not signed_sinks or not cable_ids.get("Capture"):
        raise RouteError("cable_unavailable")
    comtypes.CoInitialize()
    try:
        def lookup():
            inputs, sinks = [], []
            for index, desc in _devices():
                ptr = ctypes.c_void_p()
                if get_device(index, ctypes.byref(ptr)) or not ptr.value:
                    continue
                # PortAudio owns this reference. Take one for comtypes' managed pointer.
                endpoint = ctypes.cast(ptr, ctypes.POINTER(IMMDevice))
                endpoint.AddRef()
                eid = endpoint.GetId().lower()
                if eid == input_id and eid.startswith("{0.0.1.") and desc["max_input_channels"]:
                    inputs.append(index)
                if eid in signed_sinks and desc["max_output_channels"]>=2:
                    # Prefer the ordinary stereo render without depending on display-language.
                    rank = 0 if desc["max_output_channels"]==2 and "16 ch" not in desc["name"].lower() else 1
                    sinks.append((rank, RENDERS.index(desc["name"]) if desc["name"] in RENDERS else len(RENDERS),index))
            return inputs,sinks
        hits,sinks = lookup()
        if len(hits)!=1 or not sinks:
            # Supported on some WASAPI builds. Do not reinitialize PortAudio globally.
            if dll.PaWasapi_UpdateDeviceList()==0:
                hits,sinks = lookup()
        if len(hits)!=1 or not sinks:
            raise RouteError("restart_audio_helper")
        return hits[0],min(sinks)[2]
    finally:
        comtypes.CoUninitialize()


def write_selection(path, selected):
    profile_save.save(path, {"schema": 1, "id": selected})


def validate_selection(state):
    if state.get("schema") != 1 or (state["id"] is not None and not isinstance(state["id"], str)):
        raise ValueError("Invalid microphone selection")


class MicRoutes:
    def __init__(self, output, *, inventory=endpoint_inventory, cables=signed_cables, headsets=connected_headsets,
                 indexes=native_indexes, alias=None):
        from cable_alias import ensure
        self.alias=alias or ensure
        self.alias_error=None
        self.output = output
        self.inventory, self.cables, self.headsets, self.indexes = inventory, cables, headsets, indexes
        self.path = output.path.with_name("microphone-input.json")
        self.saved = self.error = self.active = self.owner = None
        self.usable = True
        self.cable_ids, self.connected = {}, ""
        self.cache_until = 0
        self.watch = None
        try:
            state = profile_save.load(self.path, validate_selection)
            self.saved = state["id"]
        except FileNotFoundError:
            pass
        except (OSError,ValueError,KeyError,AttributeError,TypeError):
            self.usable = False
            self.error = "saved_settings_unreadable"

    async def devices(self, force=False):
        rows = await asyncio.to_thread(self.inventory)
        if force or time.monotonic() >= self.cache_until:
            self.cable_ids, self.connected = await asyncio.gather(
                asyncio.to_thread(self.cables), asyncio.to_thread(self.headsets))
            self.cache_until = time.monotonic()+30
        return rows

    def snapshot(self, rows, token, error=None):
        ids = {r["id"] for r in rows}
        rec = recommended(rows,self.saved,self.connected)
        selected = self.saved if self.saved in ids else None if self.saved else rec
        mixer = self.output.mixer
        stream = mixer.stream if mixer is not None else None
        try:
            running = bool(self.active and stream is not None and stream.active)
            latency = stream.latency if running else None
        except (OSError,RuntimeError):
            running,latency=False,None
        source=getattr(mixer,"spotify_feed",None) if running else None
        return dict(spotifyCapture=source.state if source else "off",spotifyEdition=source.edition if source else None,
                    captureName=self.cable_ids.get("captureName"),captureAlias=self.cable_ids.get("captureAlias","unavailable"),
                    aliasError=self.alias_error,inputs=rows, mode="manual" if self.saved else "auto",savedId=self.saved,
                    recommendedId=rec,selectedId=selected,activeId=self.active if running else None,
                    routeActive=running,cableReady=bool(self.cable_ids.get("Render") and self.cable_ids.get("Capture")),
                    error=error or self.error or ("selected_input_unavailable" if self.saved and not selected else None),
                    token=token,latencyMode=mixer.latency_mode if running else None,
                    latencyMs=dict(input=round(latency[0]*1000,1),output=round(latency[1]*1000,1)) if latency else None,
                    micRms=min(1,max(0,float(mixer.mic_rms))) if running else None)

    async def stop(self, error=None):
        self.active=self.owner=None
        try:
            if self.output.mixer is not None and self.output.mixer.stream is not None:
                await asyncio.to_thread(self.output.mixer.stop)
        finally:
            self.error=error

    async def monitor(self):
        while self.active:
            await asyncio.sleep(2)
            async with self.output.lock:
                if not self.active:
                    return
                try:
                    live = await asyncio.to_thread(self.inventory)
                    if self.active not in {r["id"] for r in live}:
                        await self.stop("input_disconnected")
                    elif not self.output.mixer.stream.active:
                        await self.stop("route_interrupted")
                except Exception:
                    await self.stop("route_interrupted")

    async def perform(self, body, origin, token, rows):
        action=body.get("action")
        if not self.usable and action not in ("stop","release"):
            raise RouteError("saved_settings_unreadable")
        if action in ("stop","release"):
            if self.active:
                if action=="release" and (self.owner[0]!=origin or self.owner[2]!=body["ownerToken"]):
                    raise RouteError("route_owner_changed")
                await self.stop()
            return {}
        if action=="select":
            if self.active:
                raise RouteError("stop_before_select")
            selected=body["id"]
            if selected is not None and selected not in {r["id"] for r in rows}:
                raise RouteError("selected_input_unavailable")
            await asyncio.to_thread(write_selection,self.path,selected)
            self.saved,self.error=selected,None
            return {}
        if self.active:
            raise RouteError("route_already_active")
        if self.output.mixer is None or self.output.read_error or self.output.mixer.stream is not None:
            raise RouteError("route_unavailable")
        state=self.snapshot(rows,token)
        if not state["selectedId"]:
            raise RouteError("selected_input_unavailable" if self.saved else "no_input")
        if body["id"]!=state["selectedId"]:
            raise RouteError("selection_changed")
        if not state["cableReady"]:
            raise RouteError("cable_unavailable")
        rename=await asyncio.to_thread(self.alias,self.cable_ids)
        self.cable_ids.update(captureName=rename["captureName"],captureAlias=rename["captureAlias"])
        self.alias_error=rename["aliasError"]
        channels = await asyncio.to_thread(self.indexes,body["id"],self.cable_ids)
        try:
            await asyncio.to_thread(self.output.mixer.start_mic_route,*channels,signed_render_ids=self.cable_ids["Render"])
        except Exception:
            raise RouteError("route_start_failed") from None
        self.active, self.error = body["id"],None
        self.owner=(origin,token,secrets.token_urlsafe(32))
        if self.watch is None or self.watch.done():
            self.watch=asyncio.create_task(self.monitor())
        return {"ownerToken":self.owner[2]}

    async def handle(self, req):
        if req.headers.getall("Cookie",[]):
            return web.json_response({"error":"cookie_free_request_required"},status=403)
        origin=req.headers["Origin"]
        body={}
        if req.method=="POST":
            if req.content_type!="application/json":
                return web.json_response({"error":"expected_json"},status=415)
            try:
                body=await req.json(loads=load_json)
                action=body["action"]
                keys={"select":{"id","action"},"setup":{"id","action"},"start":{"id","action"},"stop":{"action"},"release":{"action","ownerToken"}}
                if set(body)!=keys[action] or "id" in body and not (
                    isinstance(body["id"],str) and 0<len(body["id"])<=128 or action=="select" and body["id"] is None):
                    raise ValueError()
                if action=="release" and not isinstance(body["ownerToken"],str):
                    raise ValueError()
            except (KeyError,TypeError,ValueError):
                return web.json_response({"error":"invalid_settings"},status=400)
        async with self.output.lock:
            token=self.output.token_for(origin)
            supplied=req.headers.getall("X-Lum-Mix-Token",[])
            if body and (len(supplied)!=1 or not supplied[0].isascii() or not (
                    secrets.compare_digest(token,supplied[0]) or body["action"]=="release"
                    and self.owner==(origin,supplied[0],body["ownerToken"]))):
                return web.json_response({"error":"refresh_mix_settings"},status=403)
            rows=[]
            try:
                if body.get("action") in ("release","stop"):
                    await self.perform(body,origin,supplied[0],rows)
                rows=await self.devices(force=body.get("action") in ("start","setup"))
                extra={}
                if body and body["action"] not in ("release","stop"):
                    extra=await self.perform(body,origin,token,rows)
                return web.json_response(self.snapshot(rows,token)|extra)
            except RouteError as exc:
                return web.json_response(self.snapshot(rows,token,exc.code),status=409)
            except Exception:
                error="settings_not_saved" if body.get("action")=="select" else "route_unavailable"
                return web.json_response(self.snapshot(rows,token,error),status=503)

    async def shutdown(self, app):
        async with self.output.lock:
            if self.active:
                await self.stop()
        if self.watch:
            self.watch.cancel()
            await asyncio.gather(self.watch,return_exceptions=True)

    def install(self, app):
        app.router.add_get("/mix/mic",self.handle,allow_head=False)
        app.router.add_post("/mix/mic",self.handle)
        app.on_shutdown.append(self.shutdown)

