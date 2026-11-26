"""Saved duck settings and passive status of the native mixer music bus."""
import profile_save
from aiohttp import web
from mix_gains_http import load_json
from native_mix_control import ControlError


class MixDucking:
    def __init__(self, output, mic):
        self.output, self.mic, self.control = output, mic, output.control
        self.path = output.path.with_name("mixer-ducking.json")
        profile, bad = None, False
        try:
            profile = profile_save.load(self.path, lambda data: self.control.validate_profile("duck", data))
        except FileNotFoundError:
            pass
        except (OSError, TypeError, ValueError):
            bad = True
        self.control.call("duck_load", profile=profile, bad=bad)
        if not bad:
            self.apply()

    @property
    def settings(self):
        return self.control.call("duck_get")["settings"]

    def apply(self):
        if self.output.mixer is not None:
            settings = self.settings
            self.output.mixer.set_controls(duck_enabled=settings["enabled"], duck_depth=settings["depthDb"])

    def write(self, settings):
        profile_save.save(self.path, settings)

    def snapshot(self, token):
        mixer = self.output.mixer
        try:
            active = bool(self.mic.active and mixer and mixer.stream and mixer.stream.active)
        except (RuntimeError, OSError):
            active = False
        source = getattr(mixer, "spotify_feed", None) if active else None
        present = bool(active and mixer._stage.last_music)
        controls = mixer.controls if present else None
        state = self.control.call(
            "duck_state", mixer=mixer is not None, active=active, present=present,
            reduction=mixer._stage.reduction_db if present else 0.0,
            micMuted=bool(controls and (controls.mute_master or controls.mute_mic or controls.mic_gain == 0)),
            musicMuted=bool(controls and (controls.mute_file or controls.file_gain == 0)),
        )
        return dict(state, token=token, spotifyCapture=source.state if source else "off",
                    spotifyEdition=source.edition if source else None)

    async def handle(self, request):
        if request.headers.getall("Cookie", []):
            return web.json_response({"error": "cookie_free_request_required"}, status=403)
        origin = request.headers["Origin"]
        token = self.output.token_for(origin)
        body = None
        is_post = request.method == "POST"
        if is_post:
            if self.output.authorized_token(origin, request.headers.getall("X-Lum-Mix-Token", [])) is None:
                return web.json_response({"error": "refresh_mix_settings"}, status=403)
            if request.content_type != "application/json":
                return web.Response(status=415)
            try:
                body = await request.json(loads=load_json)
            except (ValueError, TypeError):
                return web.json_response({"error": "invalid_settings"}, status=400)
        async with self.output.lock:
            if is_post:
                try:
                    proposed = self.control.call("duck_plan", body=body)
                except (ValueError, TypeError) as exc:
                    if isinstance(exc, ControlError) and str(exc) == "settings_unavailable":
                        return web.json_response({"error": "settings_unavailable"}, status=503)
                    return web.json_response({"error": "invalid_settings"}, status=400)
                if proposed is not None:
                    try:
                        self.write(proposed)
                    except OSError:
                        return web.json_response({"error": "settings_not_saved"}, status=503)
                    self.control.call("duck_commit", settings=proposed)
                    self.apply()
            elif self.control.call("duck_get")["bad"]:
                return web.json_response({"error": "settings_unavailable"}, status=503)
            return web.json_response(self.snapshot(token))

    def install(self, app):
        app.router.add_get("/mix/duck", self.handle, allow_head=False)
        app.router.add_post("/mix/duck", self.handle)
