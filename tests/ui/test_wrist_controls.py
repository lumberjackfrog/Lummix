"""Exercise the shipping XSOverlay frame and its real local asset/CSP serving."""
from http.client import HTTPConnection
from pathlib import Path
import os
import sys
import threading
import unittest
from unittest.mock import patch

from playwright.sync_api import expect, sync_playwright


WINDOWS = Path(__file__).resolve().parents[2] / "windows"
sys.path.insert(0, str(WINDOWS))
from xsoverlay_service import WristControls, make_server


class WristTests(unittest.TestCase):
    def test_live_panel_loads_rust_and_uses_host_snap_preference(self):
        gains = {"micMuted": False, "musicMuted": False, "musicGain": 157}
        writes = []

        def media(_controls, route, body=None, token=None):
            if route == "/mix/preferences":
                return {"settings": {"snap": 20}}
            if body:
                self.assertEqual(token, "fixture-only")
                writes.append(body)
                gains.update(body)
            return gains | {"token": "fixture-only"}

        with patch.object(WristControls, "media", media):
            with make_server(lambda: None, WINDOWS, port=0) as server:
                thread = threading.Thread(target=server.serve_forever, daemon=True)
                thread.start()
                try:
                    self.exercise_panel(server.server_port, writes)
                finally:
                    server.shutdown()
                    thread.join()

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
            request = route.request
            proxy = HTTPConnection("127.0.0.1", port, timeout=3)
            try:
                proxy.request(request.method, request.url.removeprefix(origin),
                              body=request.post_data_buffer,
                              headers=request.headers | {"host": "127.0.0.1:41839"})
                response = proxy.getresponse()
                route.fulfill(status=response.status, headers=dict(response.getheaders()), body=response.read())
            finally:
                proxy.close()

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
