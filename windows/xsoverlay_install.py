"""Python entry points for native XSOverlay discovery and ownership operations."""
import ctypes
from functools import cache
import json
from pathlib import Path
import sys

BEGIN = b"<!-- Lummix wrist add-on BEGIN -->"


@cache
def bindings():
    if getattr(sys, "frozen", False):
        from native_host import library
    else:
        from runtime.native_host import library
    host = library()
    discover = host.lummix_xsoverlay_discover
    discover.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
    discover.restype = ctypes.c_size_t
    addon = host.lummix_xsoverlay_addon
    addon.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint, ctypes.c_void_p, ctypes.c_size_t]
    addon.restype = ctypes.c_size_t
    return discover, addon


def discover():
    output = ctypes.create_string_buffer(262144)
    size = bindings()[0](output, len(output))
    path = json.loads(output.raw[:size]) if size else None
    return Path(path) if path else None


class AddonInstall:
    def __init__(self, discover, data_dir):
        self.discover = discover
        self.data_dir = Path(data_dir).absolute()

    def invoke(self, action):
        root = self.discover()
        data = json.dumps({"root": str(Path(root).absolute()) if root else None,
                           "data": str(self.data_dir)}).encode("ascii")
        output = ctypes.create_string_buffer(4096)
        size = bindings()[1](data, len(data), action, output, len(output))
        if not size:
            raise OSError("The native XSOverlay add-on operation did not complete.")
        return json.loads(output.raw[:size])

    def status(self):
        return self.invoke(0)

    def change(self, action):
        if action not in ("install", "remove"):
            raise ValueError("Unknown add-on action")
        return self.invoke(1 if action == "install" else 2)
