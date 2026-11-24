from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit
import json, mimetypes
from detect_devices import get_scan
import cable_setup_http

here = Path(__file__).resolve().parent
installed = None  # Set by runtime_main from the current user’s Steam libraries.
wrist = "/vendor/_UI/Default/Wrist.html"
config = Path.home() / "AppData/LocalLow/Xiexe/XSOverlay/config/overlaySettings.json"
allowed = {".js", ".css", ".woff", ".woff2", ".ttf", ".otf", ".png", ".jpg", ".webp", ".svg", ".html"}

class Renderer(BaseHTTPRequestHandler):
    def send_bytes(self, content, mime):
        self.send_response(200)
        self.send_header("Content-Type", mime + ("; charset=utf-8" if mime.startswith("text/") else ""))
        self.send_header("Content-Length", str(len(content)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Content-Security-Policy", "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; font-src 'self' data:; img-src 'self' data: blob:; connect-src 'self'; frame-ancestors http://127.0.0.1:41835 file:; base-uri 'none'; form-action 'none'")
        self.end_headers()
        self.wfile.write(content)

    def do_GET(self):
        if self.headers.get("Host") != "127.0.0.1:41836":
            self.send_error(403)
            return
        route = unquote(urlsplit(self.path).path)
        if route == "/setup-status":
            cable_setup_http.api(self, "GET")
            return
        if route == "/detect-devices":
            origin = self.headers.get("Origin")
            if origin != "http://127.0.0.1:41835":
                self.send_error(403)
                return
            content = json.dumps(get_scan()).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(content)))
            self.send_header("Access-Control-Allow-Origin", origin)
            self.send_header("Vary", "Origin")
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.end_headers()
            self.wfile.write(content)
            return
        if route == "/revision":
            versions = [str((here/name).stat().st_mtime_ns) for name in ("wrist-tile.html","wrist-fixture.js")]
            self.send_bytes("-".join(versions).encode(), "text/plain")
            return
        if route == "/preview":
            self.send_response(302)
            self.send_header("Location", wrist)
            self.end_headers()
            return
        if route == "/wrist-fixture.js":
            cfg = json.loads(config.read_bytes())
            snapshot = {key: cfg.get(key) for key in ("UseDarkTheme", "AccentColor", "MediaThemeing", "DefaultShowBatteryPercentage", "BatteryFontScale")}
            script = (here/"wrist-fixture.js").read_text(encoding="utf8")
            self.send_bytes(("const savedSettings = "+json.dumps(snapshot)+";\n"+script).encode(), "text/javascript")
            return
        if installed is None or not route.startswith("/vendor/"):
            self.send_error(404)
            return
        target = (installed / route.removeprefix("/vendor/")).resolve()
        if not target.is_relative_to(installed) or not target.is_file() or target.suffix.lower() not in allowed:
            self.send_error(404)
            return
        message = target.read_bytes()
        if route == wrist:
            extra = (here/"wrist-tile.html").read_bytes()
            message = message.replace(b"<head>", b'<head><script src="/wrist-fixture.js"></script>', 1)
            message = message.replace(b"</body>", extra+b"</body>", 1)
        mime = {".js":"text/javascript", ".css":"text/css"}.get(target.suffix, mimetypes.guess_type(target.name)[0] or "application/octet-stream")
        self.send_bytes(message, mime)

    def do_OPTIONS(self):
        if self.path == "/setup-cable":
            cable_setup_http.api(self, "OPTIONS")
        else:
            self.send_error(404)

    def do_POST(self):
        if self.path == "/setup-cable":
            cable_setup_http.api(self, "POST")
        else:
            self.send_error(404)

if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", 41836), Renderer).serve_forever()
