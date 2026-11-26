"""Exercise the shipping XSOverlay frame and its real local asset/CSP serving."""
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import os
import socket
import sys
import time
import threading
from tempfile import TemporaryDirectory
import unittest

from playwright.sync_api import expect, sync_playwright


WINDOWS = Path(__file__).resolve().parents[2] / "windows"
sys.path.insert(0, str(WINDOWS))
from xsoverlay_service import serve_with_window
from wrist_preview import serve as serve_preview


@unittest.skipUnless(sys.platform == 'win32', 'The production wrist listener uses Windows WinHTTP.')
class WristTests(unittest.TestCase):
    def test_live_panel_loads_rust_and_uses_host_snap_preference(self):
        gains = {"micMuted": False, "musicMuted": False, "musicGain": 157}
        writes = []

        class Mixer(BaseHTTPRequestHandler):
            def send(self, body, status=200):
                message = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(message)))
                self.end_headers()
                self.wfile.write(message)

            def do_GET(self):
                if self.headers.get("Origin") != "http://127.0.0.1:41835":
                    return self.send({}, 403)
                if self.path == "/mix/preferences":
                    return self.send({"settings": {"snap": 20}})
                if self.path == "/mix/gains":
                    return self.send(gains | {"token": "fixture-only"})
                return self.send({}, 404)

            def do_POST(self):
                if (self.path != "/mix/gains" or
                        self.headers.get("Origin") != "http://127.0.0.1:41835" or
                        self.headers.get("X-Lum-Mix-Token") != "fixture-only"):
                    return self.send({}, 403)
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                writes.append(body)
                gains.update(body)
                self.send(gains | {"token": "fixture-only"})

            def log_message(self, *_args):
                pass

        with ThreadingHTTPServer(("127.0.0.1", 0), Mixer) as mixer:
            thread = threading.Thread(target=mixer.serve_forever, daemon=True)
            thread.start()
            try:
                slow = None
                try:
                    with serve_with_window(lambda: None, WINDOWS, listen_port=0, mixer_port=mixer.server_port) as port:
                        self.assertIsNotNone(port, "The production native wrist listener did not start.")
                        self.check_routes(port)
                        self.exercise_panel(port, writes)
                        slow = socket.create_connection(("127.0.0.1", port), timeout=2)
                        slow.sendall(b"GET /wrist HTTP/1.1\r\n")
                        started = time.monotonic()
                    self.assertLess(time.monotonic() - started, 3.5)
                finally:
                    if slow is not None:
                        slow.close()
                stopped = HTTPConnection("127.0.0.1", port, timeout=2)
                try:
                    with self.assertRaises(OSError):
                        stopped.request("GET", "/wrist")
                finally:
                    stopped.close()
            finally:
                mixer.shutdown()
                thread.join()

    def check_routes(self, port):
        origin = "http://127.0.0.1:41835"
        host = {"Host": "127.0.0.1:41839"}
        authorized = host | {"Origin": origin}

        def request(method, path, headers, body=None):
            client = HTTPConnection("127.0.0.1", port, timeout=5)
            try:
                client.request(method, path, body=body, headers=headers)
                response = client.getresponse()
                return response.status, response.headers, response.read()
            finally:
                client.close()

        self.assertEqual(request("GET", "/wrist", {})[0], 403)
        self.assertEqual(request("GET", "/controls", host)[0], 403)
        self.assertEqual(request("GET", "/controls", host | {"Origin": "https://example.invalid", "X-Lummix-Panel": "1"})[0], 403)
        self.assertEqual(request("GET", "/lummix-addon", host)[0], 403)
        status, headers, data = request("GET", "/lummix-addon", authorized)
        self.assertEqual(status, 200)
        self.assertEqual(headers["Access-Control-Allow-Origin"], origin)
        info = json.loads(data)
        self.assertEqual(info["state"], "missing")
        config = authorized | {"Content-Type": "application/json"}
        self.assertEqual(request("POST", "/lummix-addon", config, b'{"action":"install"}')[0], 403)
        self.assertEqual(request("POST", "/lummix-addon", config | {"X-Lummix-Addon-Token": info["token"]}, b'{"action":"remove"}')[0], 409)
        cors = authorized | {"Access-Control-Request-Method": "POST", "Access-Control-Request-Headers": "content-type, x-lummix-addon-token"}
        self.assertEqual(request("OPTIONS", "/lummix-addon", cors)[0], 204)
        panel = host | {"X-Lummix-Panel": "1", "Content-Type": "application/json"}
        for body in (b"{}", b'{"musicGain":201}', b'{"musicGain":true}', b'{"unknown":1}'):
            self.assertEqual(request("POST", "/controls", panel, body)[0], 400)
        self.assertEqual(request("POST", "/controls", panel | {"Content-Length": "1024"})[0], 400)
        status, headers, data = request("GET", "/controls", panel)
        self.assertEqual(status, 200)
        self.assertNotIn("token", json.loads(data))
        self.assertEqual(headers["Cache-Control"], "no-store")

    def test_native_preview_serves_vendor_assets_and_setup_contract(self):
        actions = []
        origin = "http://127.0.0.1:41835"
        preview = "http://127.0.0.1:41836"
        parent_url = origin + "/preview-test"

        def fixture(action):
            actions.append(action)
            if action == 2:
                return {"connected": [], "saved": [], "runtimes": []}
            return {"state": "preparing" if action == 1 else "missing", "ready": False, "endpoints": None}

        with TemporaryDirectory() as temp:
            root = Path(temp)
            app = root / "Applications"
            layout = app / "_UI" / "Default"
            layout.mkdir(parents=True)
            (layout / "Wrist.html").write_text(
                '<html><head><link rel="stylesheet" href="native.css"></head>'
                '<body><h1>Vendor wrist</h1></body></html>', encoding="utf-8",
            )
            (layout / "native.css").write_text("h1 {color:rgb(90,20,210)}", encoding="utf-8")
            (layout / "private.json").write_text("{}", encoding="utf-8")
            (root / "outside.css").write_text("private", encoding="utf-8")
            config = root / "theme.json"
            config.write_text(json.dumps({"UseDarkTheme": True, "AccentColor": "#ca8fff",
                                          "unrelatedSecret": "private"}), encoding="utf-8")
            with serve_preview(WINDOWS, app, config, listen_port=0, qa=True, callback=fixture) as port:
                self.check_preview_api(port, actions)

                def browser_route(route):
                    if route.request.url == parent_url:
                        return route.fulfill(content_type="text/html", body=f"""
                            <!doctype html><title>Fixture studio</title>
                            <iframe title="Wrist preview" src="{preview}/vendor/_UI/Default/Wrist.html"></iframe>
                        """)
                    if not route.request.url.startswith(preview + "/"):
                        return route.abort()
                    self.relay(route, preview, "127.0.0.2", port)

                with sync_playwright() as playwright, playwright.chromium.launch(
                    executable_path=os.environ.get("LUMMIX_TEST_CHROMIUM")
                ) as browser, browser.new_context(service_workers="block") as context:
                    context.route("**/*", browser_route)
                    page = context.new_page()
                    page.goto(parent_url)
                    frame = page.frame_locator('iframe[title="Wrist preview"]')
                    expect(frame.get_by_text("Vendor wrist")).to_have_css("color", "rgb(90, 20, 210)")
                    expect(frame.locator("#lum-personal-level")).to_have_count(1)

    def check_preview_api(self, port, actions):
        origin = "http://127.0.0.1:41835"
        host = {"Host": "127.0.0.1:41836"}
        studio = host | {"Origin": origin}

        def request(method, path, headers=host, body=None):
            client = HTTPConnection("127.0.0.2", port, timeout=4)
            try:
                client.request(method, path, body=body, headers=headers)
                response = client.getresponse()
                return response.status, response.headers, response.read()
            finally:
                client.close()

        status, _, data = request("GET", "/lummix-health")
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(data), {"service": "lummix-wrist", "ready": True,
                                            "vendorAvailable": True, "cableRestartSupport": True})
        status, headers, _ = request("GET", "/preview")
        self.assertEqual(status, 302)
        self.assertEqual(headers["Location"], "/vendor/_UI/Default/Wrist.html")
        self.assertIn("frame-ancestors http://127.0.0.1:41835", headers["Content-Security-Policy"])
        status, _, data = request("GET", "/vendor/_UI/Default/Wrist.html")
        self.assertEqual(status, 200)
        self.assertIn((WINDOWS / "wrist-tile.html").read_bytes(), data)
        self.assertIn(b'<script src="/wrist-fixture.js"></script>', data)
        status, headers, css = request("GET", "/vendor/_UI/Default/native.css")
        self.assertEqual((status, headers["Content-Type"], css),
                         (200, "text/css; charset=utf-8", b"h1 {color:rgb(90,20,210)}"))
        self.assertEqual(request("GET", "/vendor/_UI/Default/private.json")[0], 404)
        self.assertEqual(request("GET", "/vendor/%2e%2e/outside.css")[0], 404)
        self.assertRegex(request("GET", "/revision")[2], rb"^\d+-\d+$")
        preferences = request("GET", "/wrist-fixture.js")[2].splitlines()[0]
        self.assertIn(b'"UseDarkTheme":true', preferences)
        self.assertNotIn(b"unrelatedSecret", preferences)
        self.assertEqual(request("GET", "/detect-devices")[0], 403)
        self.assertEqual(request("GET", "/detect-devices", studio)[0], 200)
        self.assertEqual(request("GET", "/setup-status")[0], 403)
        self.assertEqual(request("GET", "/setup-status", studio | {"Host":"localhost:41836"})[0], 403)
        status, headers, data = request("GET", "/setup-status", studio)
        self.assertEqual(status, 200)
        self.assertEqual(headers["Access-Control-Allow-Origin"], origin)
        old_token = json.loads(data)["token"]
        preflight = studio | {"Access-Control-Request-Method": "POST",
                              "Access-Control-Request-Headers": "Content-Type, X-Lum-Setup-Token"}
        self.assertEqual(request("OPTIONS", "/setup-cable", preflight)[0], 204)
        post = studio | {"Content-Type":"application/json","X-Lum-Setup-Token":old_token}
        self.assertEqual(request("POST", "/setup-cable", post, b'{"unexpected":true}')[0], 400)
        self.assertEqual(request("POST", "/setup-cable", post, b"{}")[0], 202)
        self.assertEqual(request("POST", "/setup-cable", post, b"{}")[0], 200)
        self.assertEqual(actions, [2, 0, 1, 0])
        fresh = json.loads(request("GET", "/setup-status", studio)[2])["token"]
        self.assertNotEqual(fresh, old_token)
        self.assertEqual(request("POST", "/setup-cable", post, b"{}")[0], 403)

    @staticmethod
    def relay(route, origin, address, port):
        request = route.request
        proxy = HTTPConnection(address, port, timeout=4)
        try:
            proxy.request(request.method, request.url.removeprefix(origin),
                          body=request.post_data_buffer,
                          headers=request.headers | {"host": origin.removeprefix("http://")})
            response = proxy.getresponse()
            route.fulfill(status=response.status, headers=dict(response.getheaders()), body=response.read())
        finally:
            proxy.close()

    def exercise_panel(self, port, writes):
        # Playwright remaps the installed panel's fixed origin to the isolated
        # test server. The real handler still receives its production Host.
        origin = "http://127.0.0.1:41839"
        parent_url = "http://localhost:41836/wrist-host"

        def offline_route(route):
            if route.request.url == parent_url:
                return route.fulfill(content_type="text/html", body=f"""
                    <!doctype html><title>XSOverlay fixture</title>
                    <style>html,body,iframe{{margin:0;border:0;width:100%;height:100%;display:block}}</style>
                    <iframe title="Lummix" src="{origin}/wrist"></iframe>
                    <script>
                    window.addEventListener('message', ({{source, data}}) => {{
                      if (data.type==='lummix-wrist-ready') source.postMessage({{
                        type:'lummix-wrist-theme',surface:'#2a2e3a',text:'#ffffff',accent:'#ca8fff'
                      }}, '{origin}')
                    }});
                    </script>
                """)
            if not route.request.url.startswith(origin + "/"):
                return route.abort()
            self.relay(route, origin, "127.0.0.1", port)

        with sync_playwright() as playwright, playwright.chromium.launch(
            executable_path=os.environ.get("LUMMIX_TEST_CHROMIUM")
        ) as browser, browser.new_context(
            viewport={"width":108,"height":180}, service_workers="block"
        ) as context:
            context.route("**/*", offline_route)
            page = context.new_page()
            errors = []
            page.on("pageerror", lambda exc: errors.append(str(exc)))
            with page.expect_response(lambda response: response.url.endswith("/step")):
                result = page.goto(parent_url)
            self.assertEqual(result.status, 200)
            panel = page.frame_locator('iframe[title="Lummix"]')
            expect(panel.locator("output")).to_have_text("157%")
            expect(panel.get_by_role("main")).to_have_css("background-color", "rgb(42, 46, 58)")
            def is_write(resp):
                return resp.url.endswith("/controls") and resp.request.method == "POST"

            mic = panel.get_by_role("button", name="Mute microphone")
            with page.expect_response(is_write):
                mic.press("Space")
            expect(panel.get_by_role("button", name="Unmute microphone")).to_have_attribute("aria-pressed", "true")
            slider = panel.get_by_role("slider", name="Music output gain")
            for key, shown in (("ArrowDown", "140%"), ("End", "200%"), ("Home", "0%")):
                with page.expect_response(is_write):
                    slider.press(key)
                expect(panel.locator("output")).to_have_text(shown)
            self.assertEqual(writes, [
                {"micMuted": True}, {"musicGain": 140}, {"musicGain": 200}, {"musicGain": 0},
            ])
            self.assertFalse(errors)
