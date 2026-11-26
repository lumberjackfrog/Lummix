"""Start or reuse identified local services and open the native studio window."""
import ctypes
from http.client import HTTPConnection
from pathlib import Path
import sys

from native_host import library, wide_path


ORIGIN = "http://127.0.0.1:41835"
SERVICES = {"studio":41835, "wrist":41836, "spotify":41838}
MEDIA_CAPABILITIES = {"spotify-session-peak-v1", "mic-route-v1", "mixer-duck-v1", "studio-preferences-v1",
                      "mix-setup-alias-v1", "cable-restart-v1", "spotify-detection-v2", "spotify-window-artwork-v1"}
if sys.getwindowsversion().build >= 20348:
    MEDIA_CAPABILITIES.add("spotify-process-mix-v1")
MEDIA_CONFLICT = ("Port 41838 is already in use by an incompatible service. Close any earlier Lummix "
                 "window or stop its media helper, then reopen this version of Lummix.")
user = ctypes.WinDLL("user32", use_last_error=True)

def request(port,path,method="GET",headers=None,limit=131072):
    conn = HTTPConnection("127.0.0.1",port,timeout=1.5)
    try:
        conn.request(method,path,headers=headers or {})
        reply=conn.getresponse()
        return reply.status,reply.headers,reply.read(limit)
    finally: conn.close()

def _failure(status, diagnostics):
    if status == 3:
        return MEDIA_CONFLICT
    if status not in (10, 11, 12, 20, 21, 22, 30, 31, 32):
        return "Windows could not start the Lummix host. Reopen Lummix and try again."
    name = ("studio", "wrist", "spotify")[status % 10]
    port = SERVICES[name]
    diagnostics.record(name + "_failed")
    if status < 20:
        return "Extract the complete Lummix ZIP into one folder and open Lummix again."
    if status < 30:
        if name == "spotify":
            return MEDIA_CONFLICT
        if name == "wrist":
            return "Port 41836 is in use. Close any older Lummix wrist preview or setup helper and reopen Lummix."
        return f"Lummix could not identify the service on port {port}. Close the conflicting app or try again after startup."
    return f"The {name} service did not start on port {port}. Extract the complete download and try again."

def launch():
    import diagnostics
    token = ctypes.c_void_p()
    host = None
    try:
        host = library()
        host.lummix_host_dpi.argtypes = []
        host.lummix_host_dpi.restype = None
        host.lummix_host_dpi()
        from studio_window import CAN_REOPEN, CLASS, OPEN, run
        host.lummix_host_start.argtypes = [ctypes.c_wchar_p, ctypes.c_size_t, ctypes.c_wchar_p,
            ctypes.c_uint32, ctypes.c_size_t, ctypes.c_uint32, ctypes.POINTER(ctypes.c_void_p)]
        host.lummix_host_start.restype = ctypes.c_uint32
        host.lummix_host_stop.argtypes = [ctypes.c_void_p]
        host.lummix_host_stop.restype = None
        helper=Path(sys.executable).resolve()
        if not getattr(sys,"frozen",False):
            helper=Path(__file__).resolve().parents[2]/".build/frozen/runtime/Lummix Helper.exe"
        status = host.lummix_host_start(*wide_path(helper), CLASS, CAN_REOPEN, OPEN,
                                        sys.getwindowsversion().build >= 20348, ctypes.byref(token))
        if status == 1:
            return
        if status:
            raise RuntimeError(_failure(status, diagnostics))
        from runtime_main import DATA, wrist_sources
        from xsoverlay_service import serve_with_window
        with serve_with_window(wrist_sources, DATA):
            diagnostics.record("opened")
            run()
            diagnostics.record("closed")
    except Exception as exc:
        diagnostics.record("startup_failed")
        user.MessageBoxW(None,str(exc),"Lummix",0x10)
        raise SystemExit(1) from exc
    finally:
        if token:
            host.lummix_host_stop(token)
