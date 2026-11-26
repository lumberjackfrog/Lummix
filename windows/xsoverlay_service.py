"""Lifetime and file-adapter bindings for the native XSOverlay panel listener."""
from contextlib import contextmanager
import ctypes
import json
import sys

from xsoverlay_install import AddonInstall


AddonCallback = ctypes.CFUNCTYPE(ctypes.c_size_t, ctypes.c_uint, ctypes.c_void_p, ctypes.c_size_t)


@contextmanager
def serve_with_window(discover, data_dir, *, listen_port=41839, mixer_port=41838, qa=False):
    if getattr(sys, "frozen", False):
        from native_host import library, wide_path
    else:
        from runtime.native_host import library, wide_path
    manager = AddonInstall(discover, data_dir)

    @AddonCallback
    def addon(action, output, capacity):
        try:
            result = manager.status() if action == 0 else manager.change({1: "install", 2: "remove"}[action])
            body = json.dumps(result, ensure_ascii=False).encode("utf-8")
            if len(body) <= capacity:
                ctypes.memmove(output, body, len(body))
                return len(body)
        except Exception:
            pass
        return 0

    try:
        dll = library()
        start = dll.lummix_wrist_start
        start.argtypes = [ctypes.c_wchar_p, ctypes.c_size_t, ctypes.c_uint16,
                          ctypes.c_uint16, AddonCallback, ctypes.c_bool, ctypes.POINTER(ctypes.c_uint16)]
        start.restype = ctypes.c_void_p
        stop = dll.lummix_wrist_stop
        stop.argtypes = [ctypes.c_void_p]
        stop.restype = None
        bound = ctypes.c_uint16()
        listener = start(*wide_path(data_dir), listen_port, mixer_port, addon, qa, ctypes.byref(bound))
    except (OSError, AttributeError):
        listener = None
    try:
        # The optional companion cannot prevent the studio from starting.
        yield bound.value if listener else None
    finally:
        if listener:
            stop(listener)
