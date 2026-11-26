"""Private executable entry point used by the Lummix desktop launcher."""
import importlib
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import sys
import threading
from urllib.parse import urlsplit
from studio_assets import studio_asset
from xsoverlay_install import discover as wrist_sources

EXECUTABLE = Path(sys.executable).resolve()
ROOT = (EXECUTABLE.parent if EXECUTABLE.stem.lower() == "lummix" else EXECUTABLE.parent.parent) if getattr(sys,"frozen",False) else Path(__file__).resolve().parents[2]
DATA = Path(getattr(sys, "_MEIPASS", Path(__file__).resolve().parent.parent))
NATIVE_HOST = EXECUTABLE.with_name("Lummix Host.exe")


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


def serve(server, linked):
    if linked:
        def on_parent_exit():
            sys.stdin.buffer.read()
            server.shutdown()
        threading.Thread(target=on_parent_exit, daemon=True).start()
    with server:
        server.serve_forever()


def helper_streams():
    """Windowed builds have no terminal. Keep the pipes passed by the parent or diagnostics."""
    import _winapi
    import msvcrt

    for name, number, mode in (("stdin", -10, "r"), ("stdout", -11, "w"), ("stderr", -12, "w")):
        if getattr(sys, name) is not None:
            continue
        try:
            handle = _winapi.GetStdHandle(number)
        except OSError as error:
            if error.winerror != 6:  # ERROR_INVALID_HANDLE
                raise
            handle = None
        if handle and handle != -1:
            process = _winapi.GetCurrentProcess()
            copy = _winapi.DuplicateHandle(process, handle, process, 0, False, _winapi.DUPLICATE_SAME_ACCESS)
            fd = msvcrt.open_osfhandle(copy, os.O_BINARY | (os.O_RDONLY if mode == "r" else os.O_WRONLY))
            stream = open(fd, mode, encoding="utf-8", errors="replace")
        else:
            stream = open(os.devnull, mode, encoding="utf-8")
        setattr(sys, name, stream)


def main():
    # The second loopback address is exclusively for release QA alongside the running studio.
    bind = "127.0.0.2" if os.getenv("LUMMIX_QA_LOOPBACK") == "2" else "127.0.0.1"
    mode = sys.argv[1:]
    linked = "--parent-stdin" in mode
    if linked:
        mode.remove("--parent-stdin")
    if mode == ["--startup"]:
        from sign_in import SignIn
        try:
            enabled = SignIn().state()["enabled"]
        except OSError:
            return  # Windows owns the sign-in decision on a managed account.
        if not enabled:
            return
        mode = ["launch"]
    if mode == ["launch"] or (not mode and EXECUTABLE.stem.lower() == "lummix"):
        from lummix_window import launch
        launch()
        return
    if mode == ["refresh-startup"]:
        from sign_in import SignIn
        try:
            SignIn().refresh_after_upgrade()
        except PermissionError:
            pass  # A policy protecting startup entries must not block the app upgrade.
        return
    if mode == ["remove-addon"]:
        from sign_in import SignIn
        from xsoverlay_install import AddonInstall
        try:
            SignIn().change(False)
        except PermissionError:
            pass
        addon = AddonInstall(wrist_sources, DATA)
        if addon.status().get("canRemove"):
            addon.change("remove")
        return
    if getattr(sys, "frozen", False):
        helper_streams()
    if mode == ["name-cable"]:
        from cable_alias import ensure
        result=ensure()
        print(json.dumps(result))
        if result["captureAlias"]!="ready":raise SystemExit(1)
    elif len(mode)==2 and mode[0]=="name-cable-worker":
        from cable_alias import worker
        raise SystemExit(worker(mode[1]))
    elif mode == ["studio"]:
        if getattr(sys, "frozen", False):
            args = [str(NATIVE_HOST), "studio"]
            if linked:
                args.append("--parent-stdin")
            result = subprocess.run(args, stdin=sys.stdin if linked else None, stdout=sys.stdout,
                                    stderr=sys.stderr, creationflags=subprocess.CREATE_NO_WINDOW)
            raise SystemExit(result.returncode)
        serve(ThreadingHTTPServer((bind,41835),Studio), linked)
    elif mode == ["wrist"]:
        from wrist_preview import run
        run(DATA, wrist_sources(), bind, linked)
    elif mode == ["addon"]:
        from xsoverlay_service import serve_with_window
        with serve_with_window(wrist_sources, DATA, qa=bind == "127.0.0.2") as port:
            if port is None:
                raise OSError("The native XSOverlay listener could not start.")
            if linked:
                sys.stdin.buffer.read()
            else:
                threading.Event().wait()
    elif mode in (["spotify"], ["media"]):
        import asyncio
        from media_http import run
        helper = importlib.import_module("spotify-helper")
        async def serve_media():
            await run(helper.create_app(), bind, linked)
        asyncio.run(serve_media())
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
        from cable_mixer import Controls
        from native_audio import Engine, ProcessCapture
        from spotify_native import provider
        from spotify_process_feed import supported
        result = dict(supported=supported(), playerFound=False, attached=False, signal=False)
        try:
            native = provider()
            target = native.call(15) if native else {"state": 4}
            if target["state"] == 1:
                result.update(playerFound=True, edition=target["edition"])
            elif target["state"] in (2, 4):
                result["error"] = "ambiguous" if target["state"] == 2 else "unavailable"
            if result["playerFound"] and result["supported"]:
                probe = ProcessCapture(Engine(Controls()), (target["pid"],target["created"],target["edition"]))
                try:
                    end = time.monotonic() + 4
                    state = probe.state
                    while state in (0, 1) and time.monotonic() < end:
                        time.sleep(.04)
                        state = probe.state
                    result.update(attached=state in (1, 2), signal=state == 2, error=state if state < 0 else None)
                finally:
                    probe.close()
        except (OSError, RuntimeError) as error:
            result["error"] = type(error).__name__
        print(json.dumps(result))
    else:
        raise SystemExit("Usage: Lummix Helper.exe studio|wrist|spotify")


if __name__ == "__main__":
    main()
