"""Hardware and process-lifetime bindings for the native wrist preview service."""
from contextlib import contextmanager
import ctypes
import json
from pathlib import Path
import sys
import threading


PreviewCallback = ctypes.CFUNCTYPE(ctypes.c_size_t, ctypes.c_uint, ctypes.c_void_p, ctypes.c_size_t)


def backend(action):
    if action == 2:
        from detect_devices import get_scan
        return get_scan()
    import cable_setup
    from cable_routes import endpoint_names

    result = dict(cable_setup.begin() if action == 1 else cable_setup.status(), endpoints=None)
    if result["ready"]:
        try:
            result["endpoints"] = endpoint_names()
        except Exception:
            pass  # The separately qualified pair remains ready if labels cannot be read.
    return result


@contextmanager
def serve(data_dir, installed, config, *, listen_port=41836, qa=False, callback=backend):
    if getattr(sys, "frozen", False):
        from native_host import library
    else:
        from runtime.native_host import library
    dll = library()
    start = dll.lummix_preview_start
    start.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint16, ctypes.c_bool,
                      PreviewCallback, ctypes.POINTER(ctypes.c_uint16)]
    start.restype = ctypes.c_void_p
    stop = dll.lummix_wrist_stop
    stop.argtypes = [ctypes.c_void_p]
    stop.restype = None

    @PreviewCallback
    def request(action, output, capacity):
        try:
            body = json.dumps(callback(action), ensure_ascii=False).encode("utf-8")
            if len(body) <= capacity:
                ctypes.memmove(output, body, len(body))
                return len(body)
        except Exception:
            pass
        return 0

    paths = json.dumps({"root": str(data_dir), "vendor": str(installed) if installed else None,
                        "config": str(config)}).encode("ascii")
    bound = ctypes.c_uint16()
    listener = start(paths, len(paths), listen_port, qa, request, ctypes.byref(bound))
    if not listener:
        raise OSError("The native wrist preview listener could not start.")
    try:
        yield bound.value
    finally:
        stop(listener)


def run(data_dir, installed, bind, linked):
    import cable_setup

    if getattr(sys, "frozen", False):
        cable_setup._controller.prepare = lambda: cable_setup.package(
            (data_dir / "VBCABLE_Driver_Pack46.zip").read_bytes)
    config = Path.home() / "AppData/LocalLow/Xiexe/XSOverlay/config/overlaySettings.json"
    with serve(data_dir, installed, config, qa=bind == "127.0.0.2"):
        if linked:
            sys.stdin.buffer.read()
        else:
            threading.Event().wait()
