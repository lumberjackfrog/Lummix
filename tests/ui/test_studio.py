"""Browser smoke tests against the real studio assets and in-memory Windows services."""
import mimetypes
import os
from pathlib import Path
import unittest
from urllib.parse import unquote, urlsplit

from playwright.sync_api import expect, sync_playwright


STUDIO = Path(__file__).resolve().parents[2] / "studio"
ORIGIN = "http://127.0.0.1:41835"
TOKEN = "fixture-mix-token"
MIC_NAME = "Lummix (VB-Audio Virtual Cable)"


class LocalServices:
    """All HTTP is fulfilled here, even when a real Lummix instance is running."""
    def __init__(self):
        self.gains = dict(micGain=102, musicGain=157, micMuted=False, musicMuted=False)
        self.devices = [dict(id="desk", name="Desk microphone", defaultInput=True),
                        dict(id="headset", name="Headset microphone", headset=True)]
        self.preferences = {}
        self.saved_mic = None
        self.started = False
        self.cable_state = "ready"
        self.cable_installs = 0
        self.addon_state = "installable"
        self.addon_commands = []
        self.commands = []
        self.unexpected = []

    def mic(self):
        selected = self.saved_mic or "desk"
        ready = self.cable_state == "ready"
        return dict(inputs=self.devices, mode="manual" if self.saved_mic else "auto",
                    savedId=self.saved_mic, recommendedId="desk", selectedId=selected,
                    activeId=selected if self.started else None, routeActive=self.started,
                    cableReady=ready, captureName=MIC_NAME if ready else None, captureAlias="ready", token=TOKEN,
                    error="cable_restart_needed" if self.cable_state=="restart-needed" else None,
                    spotifyCapture="waiting_spotify" if self.started else "off")

    def intercept(self, route):
        request = route.request
        url = urlsplit(request.url)
        path = unquote(url.path)
        headers = {"Access-Control-Allow-Origin": ORIGIN, "Access-Control-Allow-Methods":"GET, POST, OPTIONS",
                   "Access-Control-Allow-Headers":"Content-Type, X-Lum-Mix-Token, X-Lummix-Addon-Token, X-Lum-Setup-Token"}
        if url.hostname != "127.0.0.1":
            self.unexpected.append((request.method, url.hostname, path))
            return route.abort()
        if request.method == "OPTIONS":
            return route.fulfill(status=204, headers=headers)
        if url.port == 41835 and request.method == "GET":
            asset = STUDIO / (path.lstrip("/") or "index.html")
            if asset.parent == STUDIO and asset.is_file():
                return route.fulfill(path=asset, content_type=mimetypes.guess_type(asset.name)[0] or "text/plain")
        if url.port == 41839 and path == "/lummix-addon":
            if request.method == "POST":
                if request.headers.get("x-lummix-addon-token") != TOKEN:
                    return route.fulfill(status=403, headers=headers)
                action = request.post_data_json["action"]
                self.addon_commands.append(action)
                self.addon_state = "installable" if action=="remove" else "ready"
            installed = self.addon_state=="ready"
            return route.fulfill(json=dict(state=self.addon_state,name="Lummix wrist add-on",
                message="Installed in XSOverlay." if installed else "Ready to install in XSOverlay.",
                canInstall=self.addon_state=="installable",canRemove=installed,canUpdate=self.addon_state=="update",
                token=TOKEN),headers=headers)
        if url.port == 41836 and request.method == "GET":
            if path == "/preview":
                return route.fulfill(content_type="text/html",body="<title>Wrist fixture</title>")
            if path == "/setup-status":
                ready = self.cable_state == "ready"
                return route.fulfill(json=dict(state=self.cable_state, ready=ready,
                    endpoints={"capture":MIC_NAME} if ready else None, token=TOKEN), headers=headers)
            if path == "/detect-devices":
                return route.fulfill(json=dict(connected=[], saved=[], runtimes=[]), headers=headers)
        if url.port == 41836 and path == "/setup-cable" and request.method == "POST":
            if request.headers.get("x-lum-setup-token") != TOKEN or self.cable_state != "missing":
                self.unexpected.append((request.method, path))
                return route.fulfill(status=403, headers=headers)
            self.cable_installs += 1
            self.cable_state = "restart-needed"
            return route.fulfill(json=dict(state=self.cable_state, ready=False), headers=headers)
        if url.port == 41838:
            if request.method == "POST":
                body = request.post_data_json
                if request.headers.get("x-lum-mix-token") != TOKEN:
                    return route.fulfill(status=403, headers=headers)
                self.commands.append((path, body))
                if path == "/mix/gains":
                    self.gains.update(body)
                elif path == "/mix/preferences":
                    self.preferences.update(body["settings"])
                elif path == "/mix/mic":
                    action = body["action"]
                    if action == "select":
                        self.saved_mic = body["id"]
                    elif action == "setup" and body["id"] == (self.saved_mic or "desk"):
                        self.started = True
                        return route.fulfill(json=self.mic() | {"ownerToken":"fixture-owner"}, headers=headers)
                    elif action == "stop":
                        self.started = False
                    else:
                        self.unexpected.append((action,path))
                else:
                    self.unexpected.append(("POST",path))
            data = {
                "/mix/mic":self.mic(), "/mix/gains":self.gains | {"token":TOKEN,"revision":len(self.commands)},
                "/mix/preferences":{"token":TOKEN,"settings":self.preferences},
                "/mix/duck":dict(token=TOKEN, configured=True, enabled=True, depthDb=12, reductionDb=0,
                                 state="waiting_music" if self.started else "waiting_mic",
                                 spotifyCapture="waiting_spotify" if self.started else "off"),
                "/spotify/status":dict(state="no_session", installations=[], installationKnown=True),
                "/spotify/changes":dict(events=False, revision="fixture"),
                "/spotify/volume":dict(state="unavailable", volume=None, muted=False),
            }
            if path in data:
                return route.fulfill(json=data[path], headers=headers)
        self.unexpected.append((request.method,url.port,path))
        return route.abort()


class StudioTests(unittest.TestCase):
    def test_first_cable_setup_waits_for_windows_restart(self):
        services = LocalServices()
        services.cable_state = "missing"
        with sync_playwright() as playwright, playwright.chromium.launch(
            executable_path=os.environ.get("LUMMIX_TEST_CHROMIUM")
        ) as browser, browser.new_context(service_workers="block") as context:
            context.route("**/*", services.intercept)
            context.add_init_script("window.WebSocket=class extends EventTarget {close(){} send(){}};")
            page = context.new_page()
            page.goto(ORIGIN + "/?view=desktop")
            frame = page.frame_locator("#canvas")
            frame.locator("#set-up-lummix").click()
            expect(frame.locator("#mic-status-title")).to_have_text("Restart Windows to finish")
            expect(frame.locator("#cable-status")).to_contain_text("reopen Lummix from Start")
            expect(frame.locator("#mic-live-action")).to_be_disabled()
            self.assertEqual(services.cable_installs, 1)
            self.assertFalse([body for path,body in services.commands if path=="/mix/mic"])
            page.reload()
            frame.locator("#set-up-lummix").click()
            expect(frame.locator("#mic-status-title")).to_have_text("Restart Windows to finish")
            self.assertEqual(services.cable_installs, 1)
            self.assertFalse([body for path,body in services.commands if path=="/mix/mic"])

            services.cable_state = "ready"
            page.reload()
            frame.locator("#set-up-lummix").click()
            expect(frame.locator("#mic-live-action")).to_have_text("Stop mix")
            expect(frame.locator("#mic-endpoint")).to_have_text(MIC_NAME)
            self.assertEqual(services.cable_installs, 1)
            self.assertIn(("/mix/mic", {"action":"setup", "id":"desk"}), services.commands)
            self.assertFalse(services.unexpected)

    def test_studio(self):
        with sync_playwright() as pw:
            browser = pw.chromium.launch(executable_path=os.environ.get("LUMMIX_TEST_CHROMIUM"))
            try:
                for width,height in [(1507,920),(390,844)]:
                    with self.subTest(width=width):
                        self.run_at_size(browser,width,height)
            finally:
                browser.close()

    def run_at_size(self,browser,width,height):
        services = LocalServices()
        context = browser.new_context(viewport={"width":width,"height":height}, service_workers="block")
        try:
            context.route("**/*",services.intercept)
            # The optional XSOverlay integration may open a local WebSocket; keep it isolated too.
            context.add_init_script("""
                window.WebSocket=class extends EventTarget {close(){} send(){}};
                if(navigator.mediaDevices)navigator.mediaDevices.getUserMedia=()=>{throw Error('Fixture refuses microphone capture');};
            """)
            page=context.new_page()
            page.goto(ORIGIN+"/?view=desktop")
            frame=page.frame_locator("#canvas")
            settings=frame.get_by_role("button",name="Settings",exact=True)
            setup=frame.locator("#set-up-lummix")
            mic=frame.locator("#mic")
            music=frame.locator("#music")
            expect(setup).to_have_accessible_name("Set up Lummix")
            expect(frame.locator("#window-controls")).to_be_hidden()
            mixer=frame.locator(".mixer")
            expect(mixer).to_have_css("border-top-width","1px")
            self.assertGreater(mixer.bounding_box()["x"],0)
            expect(frame.locator("#connect-spotify [data-glyph=music] svg")).to_be_visible()
            expect(frame.locator(".connections button")).to_have_count(2)
            expect(frame.locator(".connections [data-glyph=settings]")).to_have_count(0)
            expect(mic).to_have_value("102")
            expect(music).to_have_value("157")
            self.assertFalse(services.commands)
            a,b=setup.bounding_box(),frame.locator("#connect-spotify").bounding_box()
            self.assertLess(abs(a["width"]-b["width"]),2)
            self.assertLess(abs(a["height"]-b["height"]),1)
            self.assertGreaterEqual(a["height"],44)
            settings.focus()
            settings.press("Enter")
            options=frame.get_by_role("combobox",name="Microphone input")
            expect(options).to_have_value("")
            expect(options.locator("option")).to_have_count(3)
            self.assertFalse(services.commands)
            self.assertFalse(services.addon_commands)
            panel=frame.get_by_role("region",name="XSOverlay",exact=True)
            expect(panel.get_by_role("button",name="Install add-on")).to_be_visible()
            panel.get_by_role("button",name="Install add-on").click()
            expect(panel.get_by_role("status")).to_have_text("Installed in XSOverlay.")
            expect(panel.get_by_text("Restart XSOverlay",exact=False)).to_be_visible()
            panel.get_by_role("button",name="Remove add-on").click()
            expect(panel.get_by_role("button",name="Install add-on")).to_be_visible()
            services.addon_state="update"
            panel.get_by_role("button",name="Check again").click()
            panel.get_by_role("button",name="Update add-on").click()
            expect(panel.get_by_role("status")).to_have_text("Installed in XSOverlay.")
            self.assertEqual(services.addon_commands,["install","remove","install"])
            options.select_option("headset")
            expect(frame.locator("#mic-source-note")).to_contain_text("Saved on this PC")
            snap=frame.locator("#slider-snapping")
            snap.select_option("5")
            page.keyboard.press("Escape")
            with page.expect_response(lambda r:r.url.endswith("/mix/gains") and r.request.method=="POST"):
                mic.press("ArrowDown")
            expect(mic).to_have_value("100")
            self.assertEqual(services.gains["micGain"],100)
            self.assertEqual(services.gains["musicGain"],157)
            with page.expect_response(lambda r:r.url.endswith("/mix/gains") and r.request.method=="POST"):
                music.press("ArrowUp")
            expect(music).to_have_value("160")
            self.assertEqual(services.gains["micGain"],100)
            self.assertEqual(services.gains["musicGain"],160)
            page.reload()
            settings.click()
            expect(snap).to_have_value("5")
            expect(options).to_have_value("headset")
            page.keyboard.press("Escape")
            setup.focus()
            setup.press("Enter")
            expect(frame.locator("#mic-live-action")).to_have_text("Stop mix")
            expect(frame.locator("#mic-endpoint")).to_have_text(MIC_NAME)
            self.assertIn(("/mix/mic",{"action":"setup","id":"headset"}),services.commands)
            page.keyboard.press("Escape")
            expect(setup).to_have_accessible_name("Stop mix")
            setup.click()
            expect(frame.locator("#mic-live-status")).to_have_text("Mix stopped")
            self.assertFalse(services.started)
            self.assertEqual(services.unexpected,[])
        finally:
            context.close()

    def test_native_window_controls(self):
        with sync_playwright() as pw:
            browser = pw.chromium.launch(executable_path=os.environ.get("LUMMIX_TEST_CHROMIUM"))
            try:
                for width in (1450,390,320):
                    with self.subTest(width=width):
                        self.verify_chrome(browser,width)
            finally:
                browser.close()

    def verify_chrome(self,browser,width):
        # Exercise the one-time migration of a legacy appearance into an empty native store.
        legacy_storage={"origins":[{"origin":ORIGIN,"localStorage":[
            {"name":"lum-mix-appearance","value":'{"follow":false,"dark":true}'}]}],"cookies":[]}
        context=browser.new_context(viewport={"width":width,"height":865},has_touch=width<500,
                                    service_workers="block",storage_state=legacy_storage)
        try:
            services=LocalServices()
            context.route("**/*",services.intercept)
            context.add_init_script("""
              window.WebSocket=class extends EventTarget {close(){} send(){}};
              const bridge=new EventTarget(); window.chrome ||= {}; window.chrome.webview=bridge;
              window.commands=[]; let maximized=false, fullscreen=false;
              bridge.postMessage=command=>{
                window.commands.push(command);
                if(command==='maximize')maximized=!maximized;
                if(command==='fullscreen')fullscreen=!fullscreen;
                bridge.dispatchEvent(new MessageEvent('message',{data:{type:'lummix-window-state',maximized,fullscreen}}));
              };
            """)
            page=context.new_page()
            with page.expect_response(lambda r:r.url.endswith("/mix/preferences") and r.request.method=="POST"):
                page.goto(ORIGIN+"/?view=desktop&app=1")
            frame=page.frame_locator("#canvas")
            group=frame.get_by_role("group",name="Window controls")
            expect(group).to_be_visible()
            mixer=frame.locator(".mixer")
            expect(mixer).to_have_css("border-top-width","0px")
            expect(mixer).to_have_css("border-top-left-radius","0px")
            expect(mixer).to_have_css("box-shadow","none")
            bounds=mixer.bounding_box()
            self.assertEqual((bounds["x"],bounds["y"],bounds["width"]),(0,0,width))
            self.assertGreaterEqual(bounds["height"],865)
            transport=frame.locator(".transport-panel")
            expect(transport).to_have_css("border-top-width","1px")
            self.assertGreater(transport.bounding_box()["x"],0)
            self.assertEqual(group.get_by_role("button").count(),4)
            settings=frame.get_by_role("button",name="Settings",exact=True)
            expect(settings).to_be_visible()
            gear=settings.bounding_box()
            first=group.get_by_role("button",name="Minimize window")
            self.assertGreaterEqual(first.bounding_box()["x"],gear["x"]+gear["width"])
            for button in group.get_by_role("button").all():
                box=button.bounding_box()
                self.assertGreaterEqual(box["width"],44 if width<500 else 34)
                self.assertLessEqual(box["x"]+box["width"],width)
                self.assertEqual(box["y"],first.bounding_box()["y"])
            settings.focus()
            page.keyboard.press("Tab")
            expect(first).to_be_focused()
            first.press("Enter")
            group.get_by_role("button",name="Maximize window").click()
            group.get_by_role("button",name="Restore window").press("Space")
            group.get_by_role("button",name="Enter fullscreen").click()
            group.get_by_role("button",name="Leave fullscreen").click()
            group.get_by_role("button",name="Close window").click()
            drag=frame.locator("#window-drag")
            drag.dblclick()
            rect=drag.bounding_box()
            self.assertGreaterEqual(rect["width"],14)
            x,y=rect["x"]+rect["width"]/2,rect["y"]+rect["height"]/2
            page.mouse.move(x,y);page.mouse.down();page.mouse.move(x+7,y);page.mouse.up()
            self.assertEqual(page.evaluate("window.commands"),
                             ["window-state","minimize","maximize","maximize","fullscreen","fullscreen","close","maximize","drag"])
            with page.expect_response(lambda r:r.url.endswith("/mix/preferences") and r.request.method=="GET"):
                page.goto(ORIGIN+"/?view=wrist&app=1")
            expect(frame.locator("#close-to-tray")).to_be_enabled()
            expect(frame.locator("#window-controls")).to_be_hidden()
            self.assertEqual(services.commands,[
                ("/mix/preferences",{"settings":{"followTheme":False,"dark":True},"migrate":True})])
            self.assertEqual(services.addon_commands,[])
            self.assertFalse(services.unexpected)
        finally:
            context.close()


if __name__ == "__main__":
    unittest.main()
