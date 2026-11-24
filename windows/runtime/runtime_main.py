"""Private executable entry point used by the Lummix desktop launcher."""
import importlib
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import re
import sys
import threading
from urllib.parse import urlsplit
from studio_assets import STUDIO_FILES, studio_asset

EXECUTABLE = Path(sys.executable).resolve()
ROOT = (EXECUTABLE.parent if EXECUTABLE.stem.lower() == "lummix" else EXECUTABLE.parent.parent) if getattr(sys,"frozen",False) else Path(__file__).resolve().parents[2]
DATA = Path(getattr(sys, "_MEIPASS", Path(__file__).resolve().parent.parent))


class Studio(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.headers.get_all("Host") != ["127.0.0.1:41835"]:
            self.send_error(403)
            return
        route = urlsplit(self.path).path
        if route == "/lummix-health":
            data, mime = b'{"service":"lummix-studio","ready":true}', "application/json"
        else:
            asset = studio_asset(ROOT / "studio", route)
            if not asset:
                self.send_error(404)
                return
            path, mime = asset
            if not path.is_file():
                self.send_error(404)
                return
            data = path.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", mime)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)


def wrist_sources():
    import winreg
    steam = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Steam"
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Valve\Steam") as key:
            steam = Path(winreg.QueryValueEx(key, "SteamPath")[0])
    except OSError:
        pass
    roots = [steam]
    vdf = steam / "steamapps/libraryfolders.vdf"
    if vdf.is_file():
        roots.extend(Path(value.replace("\\\\", "\\")) for value in re.findall(
            r'"path"\s*"([^"]+)"', vdf.read_text(encoding="utf-8", errors="replace")))
    for library in roots:
        for product in ("XSOverlay_Beta","XSOverlay"):
            directory = library / "steamapps/common" / product / "XSOverlay_Data/StreamingAssets/Plugins/Applications"
            if (directory/"_UI/Default/Wrist.html").is_file():
                return directory.resolve()
    return None


def serve(server, linked):
    if linked:
        def on_parent_exit():
            sys.stdin.buffer.read()
            server.shutdown()
        threading.Thread(target=on_parent_exit, daemon=True).start()
    with server:
        server.serve_forever()


def start_wrist(bind, linked):
    import cable_setup
    wrist = importlib.import_module("wrist-server")
    wrist.here = DATA
    installed = wrist_sources()
    wrist.installed = installed
    if getattr(sys,"frozen",False):
        cable_setup._controller.prepare = lambda: cable_setup.package((DATA/"VBCABLE_Driver_Pack46.zip").read_bytes)
    class LocalWrist(wrist.Renderer):
        def log_message(self,*args):
            pass

        def do_GET(self):
            if self.headers.get_all("Host") != ["127.0.0.1:41836"]:
                self.send_error(403)
                return
            route = urlsplit(self.path).path
            if route == "/lummix-health":
                self.send_bytes(json.dumps({"service":"lummix-wrist","ready":True,
                                           "vendorAvailable":bool(installed)}).encode(), "application/json")
                return
            if route == "/preview" and not installed:
                self.send_bytes(b'<!doctype html><meta charset="utf-8"><style>html{color-scheme:dark;font:16px system-ui}body{padding:1em}</style><p>Wrist preview requires XSOverlay on this PC.</p>', "text/html")
                return
            if route == "/wrist-fixture.js" and not wrist.config.is_file():
                self.send_bytes(b"const savedSettings = {};\n" + (DATA/"wrist-fixture.js").read_bytes(), "text/javascript")
                return
            super().do_GET()
    serve(ThreadingHTTPServer((bind,41836),LocalWrist), linked)


def main():
    # The second loopback address is exclusively for release QA alongside the running studio.
    bind = "127.0.0.2" if os.getenv("LUMMIX_QA_LOOPBACK") == "2" else "127.0.0.1"
    mode = sys.argv[1:]
    linked = "--parent-stdin" in mode
    if linked:
        mode.remove("--parent-stdin")
    if mode == ["launch"] or (not mode and EXECUTABLE.stem.lower() == "lummix"):
        from lummix_window import launch
        launch()
    elif mode == ["name-cable"]:
        from cable_alias import ensure
        result=ensure()
        print(json.dumps(result))
        if result["captureAlias"]!="ready":raise SystemExit(1)
    elif len(mode)==2 and mode[0]=="name-cable-worker":
        from cable_alias import worker
        raise SystemExit(worker(mode[1]))
    elif mode == ["studio"]:
        serve(ThreadingHTTPServer((bind,41835),Studio), linked)
    elif mode == ["wrist"]:
        start_wrist(bind, linked)
    elif mode == ["addon"]:
        from xsoverlay_service import make_server
        serve(make_server(wrist_sources, DATA, bind), linked)
    elif mode == ["remove-addon"]:
        from xsoverlay_install import AddonInstall
        addon = AddonInstall(wrist_sources, DATA)
        if addon.status().get("canRemove"):
            addon.change("remove")
    elif mode == ["spotify"]:
        helper = importlib.import_module("spotify-helper")
        if linked:
            import asyncio
            async def until_closed():
                runner = helper.web.AppRunner(helper.create_app(), access_log=None)
                await runner.setup()
                try:
                    await helper.web.TCPSite(runner,bind,41838).start()
                    await asyncio.to_thread(sys.stdin.buffer.read)
                finally:
                    await runner.cleanup()
            asyncio.run(until_closed())
        else:
            helper.web.run_app(helper.create_app(),host=bind,port=41838,access_log=None,print=None)
    elif len(mode) == 1 and Path(mode[0]).resolve() == (DATA/"detect_openvr.py").resolve():
        from detect_openvr import devices
        print(json.dumps(devices()))
    elif mode == ["verify-vendor"]:
        import cable_setup
        with cable_setup.package((DATA/"VBCABLE_Driver_Pack46.zip").read_bytes):
            print("Original signed VB-CABLE archive and catalogs verified.")
    elif mode == ["check-spotify"]:
        # A bounded capture check for support. Never open a mic, deliver audio, or touch playback.
        import time
        from spotify_process_feed import AmbiguousTarget, Capture, spotify_target, supported
        result = dict(supported=supported(), playerFound=False, attached=False, signal=False)
        try:
            target = spotify_target()
            if target:
                result.update(playerFound=True, edition=target[2])
            if target and result["supported"]:
                probe = Capture(target)
                try:
                    end = time.monotonic() + 4
                    while time.monotonic() < end and probe.code is None and not probe.last_signal:
                        time.sleep(.04)
                    result.update(attached=probe.attached, signal=bool(probe.last_signal), error=probe.code)
                finally:
                    probe.close()
        except (AmbiguousTarget, OSError) as error:
            result["error"] = type(error).__name__
        print(json.dumps(result))
    else:
        raise SystemExit("Usage: Lummix Helper.exe studio|wrist|spotify")


if __name__ == "__main__":
    main()
