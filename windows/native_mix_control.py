"""Bindings for Rust-owned mixer settings, mic selection and capabilities."""
import ctypes as ct
from functools import lru_cache
import json
from weakref import finalize

from native_audio import library


class ControlError(ValueError):
    pass


@lru_cache(maxsize=1)
def bindings():
    dll = library()
    for name, parameters, result in (
        ("new", (), ct.c_void_p),
        ("call", (ct.c_void_p, ct.c_char_p), ct.c_void_p),
        ("free", (ct.c_void_p,), None),
        ("free_json", (ct.c_void_p,), None),
    ):
        function = getattr(dll, "lummix_control_" + name)
        function.argtypes = parameters
        function.restype = result
    return dll


class ControlPlane:
    def __init__(self):
        self.dll = bindings()
        self.handle = self.dll.lummix_control_new()
        if not self.handle:
            raise OSError("Mixer control state is unavailable")
        self._free = finalize(self, self.dll.lummix_control_free, self.handle)

    def call(self, operation, **fields):
        request = json.dumps({"op": operation, **fields}, ensure_ascii=True, allow_nan=False).encode("ascii")
        reply = self.dll.lummix_control_call(self.handle, request)
        if not reply:
            raise OSError("Mixer control state is unavailable")
        try:
            result = json.loads(ct.string_at(reply))
        finally:
            self.dll.lummix_control_free_json(reply)
        if "error" in result:
            raise ControlError(result["error"])
        return result["value"]

    def validate_profile(self, kind, data):
        if not self.call("validate_profile", kind=kind, profile=data):
            raise ValueError("Invalid saved mixer settings")
