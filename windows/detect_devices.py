"""Shell adapter for the native read-only Windows VR inventory."""
import ctypes
from functools import cache
import json
from pathlib import Path
import sys


@cache
def discovery():
    if getattr(sys, "frozen", False):
        from native_host import library
    else:
        from runtime.native_host import library
    dll = library()
    scan = dll.lummix_vr_scan
    scan.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t]
    scan.restype = ctypes.c_size_t
    return scan


def get_scan():
    script = Path(__file__).resolve().with_name("detect_openvr.py")
    config = json.dumps({"exe": sys.executable, "script": str(script)}).encode("ascii")
    output = ctypes.create_string_buffer(32768)
    size = discovery()(config, len(config), output, len(output))
    if not size:
        raise OSError("The native Windows VR inventory is unavailable.")
    return json.loads(output.raw[:size])


if __name__ == "__main__":
    print(json.dumps(get_scan(), ensure_ascii=True))
