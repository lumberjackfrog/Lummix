"""Saved duck settings and passive status of the native mixer music bus."""
import json
import os
import secrets
import tempfile
from pathlib import Path
from aiohttp import web
from mix_gains_http import load_json


def validate(patch):
    if not isinstance(patch, dict) or not patch or patch.keys() - {"enabled","depthDb"}:
        raise ValueError()
    if "enabled" in patch and type(patch["enabled"]) is not bool:
        raise ValueError()
    if "depthDb" in patch and (type(patch["depthDb"]) is not int or not 1 <= patch["depthDb"] <= 24):
        raise ValueError()
    return patch


class MixDucking:
    def __init__(self, output, mic):
        self.output, self.mic = output, mic
        self.path = output.path.with_name("mixer-ducking.json")
        self.settings = {"enabled":True,"depthDb":12}
        self.configured = self.bad_file = False
        try:
            self.settings.update(validate(load_json(self.path.read_text("utf-8"))))
            self.configured = True
        except FileNotFoundError:
            pass
        except (OSError,TypeError,ValueError):
            self.bad_file = True
        if not self.bad_file:
            self.apply()

    def apply(self):
        if self.output.mixer is not None:
            self.output.mixer.set_controls(duck_enabled=self.settings["enabled"],duck_depth=self.settings["depthDb"])

    def write(self, settings):
        self.path.parent.mkdir(parents=True,exist_ok=True)
        name = None
        try:
            with tempfile.NamedTemporaryFile("w",encoding="utf-8",delete=False,dir=self.path.parent) as f:
                name = f.name
                json.dump(settings,f)
                f.flush()
                os.fsync(f.fileno())
            os.replace(name,self.path)
        finally:
            if name:
                Path(name).unlink(missing_ok=True)

    def snapshot(self,token):
        mixer = self.output.mixer
        try:
            active = bool(self.mic.active and mixer and mixer.stream and mixer.stream.active)
        except (RuntimeError,OSError):
            active = False
        source=getattr(mixer,"spotify_feed",None) if active else None
        present = bool(active and mixer._stage.last_music)
        reduction = mixer._stage.duck.amount if present else 0.
        if mixer is None:
            state = "unavailable"
        elif not self.settings["enabled"]:
            state = "off"
        elif not active:
            state = "waiting_mic"
        elif not present:
            state = "waiting_music"
        elif mixer.controls.mute_master or mixer.controls.mute_mic or mixer.controls.mic_gain == 0:
            state = "mic_muted"
        elif mixer.controls.mute_file or mixer.controls.file_gain == 0:
            state = "music_muted"
        else:
            state = "ducking" if reduction >= .1 else "ready"
        return dict(self.settings, configured=self.configured,token=token,state=state,
                    spotifyCapture=source.state if source else "off",spotifyEdition=source.edition if source else None,
                    source="mixer_music" if present else None,reductionDb=round(reduction,2))

    async def handle(self,request):
        if request.headers.getall("Cookie",[]):
            return web.json_response({"error":"cookie_free_request_required"},status=403)
        token=self.output.token_for(request.headers["Origin"])
        patch=None
        migrate=False
        if request.method=="POST":
            keys=request.headers.getall("X-Lum-Mix-Token",[])
            if len(keys)!=1 or not keys[0].isascii() or not secrets.compare_digest(token,keys[0]):
                return web.json_response({"error":"refresh_mix_settings"},status=403)
            if request.content_type!="application/json":
                return web.Response(status=415)
            try:
                body=await request.json(loads=load_json)
                if not isinstance(body,dict):
                    raise ValueError()
                migrate=body.pop("migrate",False)
                if type(migrate) is not bool:
                    raise ValueError()
                patch=validate(body)
            except (ValueError,TypeError):
                return web.json_response({"error":"invalid_settings"},status=400)
        async with self.output.lock:
            if self.bad_file:
                return web.json_response({"error":"settings_unavailable"},status=503)
            if patch is not None and not (migrate and self.configured):
                updated=self.settings | patch
                if updated!=self.settings or not self.configured:
                    try:
                        self.write(updated)
                    except OSError:
                        return web.json_response({"error":"settings_not_saved"},status=503)
                    self.settings,self.configured=updated,True
                    self.apply()
            return web.json_response(self.snapshot(token))

    def install(self,app):
        app.router.add_get("/mix/duck",self.handle,allow_head=False)
        app.router.add_post("/mix/duck",self.handle)
