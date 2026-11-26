"""Private shell entry for the native background-only OpenVR probe."""
import ctypes
import json
import sys


def devices():
    if getattr(sys, "frozen", False):
        from native_host import library
    else:
        from runtime.native_host import library
    read = library().lummix_openvr_devices
    read.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
    read.restype = ctypes.c_size_t
    output = ctypes.create_string_buffer(8192)
    size = read(output, len(output))
    if not size:
        raise OSError("No running compatible SteamVR server.")
    return json.loads(output.raw[:size])


if __name__ == "__main__":
    try:
        print(json.dumps(devices(), ensure_ascii=True))
    except OSError:
        sys.exit(1)
