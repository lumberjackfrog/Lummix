"""Local XSOverlay settings and the wrist's three outgoing audio controls."""
from contextlib import contextmanager
from http.client import HTTPConnection, HTTPException
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import secrets
import threading
from urllib.parse import urlsplit

from xsoverlay_install import AddonInstall

DESKTOP = "http://127.0.0.1:41835"
PANEL = "http://127.0.0.1:41839"
GAIN_KEYS = ("micMuted", "musicMuted", "musicGain")


class WristControls:
    def __init__(self, address):
        self.address = address
        self.lock = threading.Lock()

    def media(self, route, patch=None, token=None):
        conn = HTTPConnection(self.address, 41838, timeout=2)
        headers = {"Host": "127.0.0.1:41838", "Origin": DESKTOP}
        if token:
            headers.update({"X-Lum-Mix-Token": token, "Content-Type": "application/json"})
        try:
            conn.request("POST" if patch else "GET", route,
                         body=json.dumps(patch) if patch else None, headers=headers)
            reply = conn.getresponse()
            if reply.status != 200:
                raise OSError(f"Mixer returned {reply.status}")
            return json.loads(reply.read(512_000))
        finally:
            conn.close()

    def update(self, patch=None):
        with self.lock:
            values = self.media("/mix/gains")
            if patch:
                values = self.media("/mix/gains", patch, values["token"])
            return {key: values[key] for key in GAIN_KEYS}


def valid_gain(body):
    return (isinstance(body, dict) and bool(body) and not (body.keys() - set(GAIN_KEYS))
            and all((type(value) is int and 0 <= value <= 200) if key == "musicGain"
                    else type(value) is bool for key, value in body.items()))


def make_server(discover, data_dir, bind="127.0.0.1", port=41839):
    manager = AddonInstall(discover, data_dir)
    lock = threading.Lock()
    controls = WristControls(bind)
    install_token = secrets.token_urlsafe(32)
    panel_html = (data_dir / "xsoverlay-panel.html").read_bytes()
    # Frozen builds carry their own copy; source runs share the Studio assets.
    controls_dir = data_dir if (data_dir / "controls.wasm").is_file() else data_dir.parent / "studio"
    panel_assets = {"/controls-bridge.js": "text/javascript", "/controls.wasm": "application/wasm"}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def permit(self, config=False):
            if self.headers.get_all("Host") != ["127.0.0.1:41839"]:
                return False
            origins = self.headers.get_all("Origin", [])
            if config:
                return origins == [DESKTOP]
            return origins in ([], [PANEL]) and self.headers.get("X-Lummix-Panel") == "1"

        def reply(self, status, value, mime="application/json", config=False):
            data = value if isinstance(value, bytes) else json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Type", mime)
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            if config:
                self.send_header("Access-Control-Allow-Origin", DESKTOP)
                self.send_header("Vary", "Origin")
            if mime.startswith("text/html"):
                self.send_header("Content-Security-Policy", "default-src 'none'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'self' file: http://localhost:* http://127.0.0.1:*")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            if data:
                self.wfile.write(data)

        def do_OPTIONS(self):
            if (self.path != "/lummix-addon" or not self.permit(config=True)
                    or self.headers.get("Access-Control-Request-Method") != "POST"
                    or set(k.strip().lower() for k in self.headers.get("Access-Control-Request-Headers", "").split(",")) != {"content-type", "x-lummix-addon-token"}):
                return self.reply(403, {})
            self.send_response(204)
            self.send_header("Access-Control-Allow-Origin", DESKTOP)
            self.send_header("Access-Control-Allow-Methods", "POST")
            self.send_header("Access-Control-Allow-Headers", "Content-Type, X-Lummix-Addon-Token")
            self.send_header("Vary", "Origin, Access-Control-Request-Headers, Access-Control-Request-Method")
            self.end_headers()

        def do_GET(self):
            if self.headers.get_all("Host") != ["127.0.0.1:41839"]:
                return self.reply(403, {})
            path = urlsplit(self.path).path
            if path == "/wrist":
                return self.reply(200, panel_html, "text/html; charset=utf-8")
            if path in panel_assets:
                try:
                    data = (controls_dir / path.removeprefix("/")).read_bytes()
                except OSError:
                    return self.reply(404, {})
                return self.reply(200, data, panel_assets[path])
            if path == "/lummix-addon" and self.permit(config=True):
                with lock:
                    result = manager.status()
                return self.reply(200, {**result, "token": install_token}, config=True)
            if path in ("/controls", "/step") and self.permit():
                try:
                    if path == "/step":
                        settings = controls.media("/mix/preferences").get("settings", {})
                        value = settings.get("snap", 10)
                        return self.reply(200, {"step": value if value in (5, 10, 20) else 1})
                    return self.reply(200, controls.update())
                except (OSError, HTTPException, ValueError, KeyError):
                    return self.reply(503, {"error": "Mixer unavailable"})
            return self.reply(403 if path in ("/lummix-addon", "/controls", "/step") else 404, {})

        def do_POST(self):
            config = self.path == "/lummix-addon"
            if self.path not in ("/lummix-addon", "/controls") or not self.permit(config):
                return self.reply(403, {})
            if config and self.headers.get_all("X-Lummix-Addon-Token") != [install_token]:
                return self.reply(403, {}, config=True)
            if self.headers.get_content_type() != "application/json":
                return self.reply(415, {}, config=config)
            try:
                size = int(self.headers.get("Content-Length", "0"))
                if not 0 < size <= 512:
                    raise ValueError
                body = json.loads(self.rfile.read(size))
            except (ValueError, UnicodeError):
                return self.reply(400, {}, config=config)
            if config:
                if body not in ({"action": "install"}, {"action": "remove"}):
                    return self.reply(400, {}, config=True)
                with lock:
                    result = manager.change(body["action"])
                success = result["state"] == ("ready" if body["action"] == "install" else "installable")
                return self.reply(200 if success else 409, {**result, "token": install_token}, config=True)
            if not valid_gain(body):
                return self.reply(400, {})
            try:
                return self.reply(200, controls.update(body))
            except (OSError, HTTPException, KeyError, ValueError):
                return self.reply(503, {"error": "Mixer unavailable"})

    return ThreadingHTTPServer((bind, port), Handler)


@contextmanager
def serve_with_window(discover, data_dir):
    # An optional companion must never prevent the main audio mixer from starting.
    try:
        server = make_server(discover, data_dir)
    except OSError:
        yield
        return
    thread = threading.Thread(target=server.serve_forever, name="XSOverlay controls", daemon=True)
    thread.start()
    try:
        yield
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
