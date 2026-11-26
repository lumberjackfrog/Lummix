"""Marshal the private worker command to the Windows Rust host."""
import ctypes
from functools import cache
import subprocess
import sys

if getattr(sys, "frozen", False):
    from native_host import library, wide_path
else:
    from runtime.native_host import library, wide_path


@cache
def _run():
    call = library().lummix_cable_run_private
    call.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_size_t,
                     ctypes.c_uint32, ctypes.POINTER(ctypes.c_uint32)]
    call.restype = ctypes.c_uint32
    return call


def private_run(exe, args, timeout=18):
    command = subprocess.list2cmdline([str(exe), *map(str, args)])
    code = ctypes.c_uint32()
    status = _run()(str(exe), *wide_path(command), timeout * 1000, ctypes.byref(code))
    if status == 1460:
        raise TimeoutError("Windows Sound did not respond")
    if status:
        raise ctypes.WinError(status)
    return code.value
