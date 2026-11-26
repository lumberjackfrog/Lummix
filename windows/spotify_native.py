"""Adapt Lummix's Rust Windows media provider to the HTTP service's session interface."""
import asyncio
import base64
from concurrent.futures import ThreadPoolExecutor
import ctypes
from functools import lru_cache
from itertools import count
import json
from pathlib import Path
import os
import sys
from types import SimpleNamespace
from weakref import finalize


_CALLBACK = ctypes.CFUNCTYPE(None, ctypes.c_uint64)
_PLAYBACK_NAMES = ("CLOSED", "OPENED", "CHANGING", "STOPPED", "PLAYING", "PAUSED")


@lru_cache(maxsize=1)
def provider():
    if sys.platform != "win32":
        return None
    if getattr(sys, "frozen", False):
        path = Path(sys._MEIPASS) / "lummix_media.dll"
    else:
        override = os.environ.get("LUMMIX_MEDIA_DLL")
        path = Path(override).resolve() if override else (
            Path(__file__).resolve().parents[1] / ".build/media_native/release/lummix_media.dll"
        )
        if not path.is_file() and not override:
            return None
    return NativeMedia(ctypes.CDLL(str(path)))


class NativeMedia:
    def __init__(self, dll):
        self.dll = dll
        dll.lummix_media_abi.argtypes = ()
        dll.lummix_media_abi.restype = ctypes.c_uint32
        if dll.lummix_media_abi() != 1:
            raise RuntimeError("The Lummix media library has an incompatible version.")
        for name, types in {
            "call": (ctypes.c_uint32, ctypes.c_uint64, ctypes.c_uint32, ctypes.c_int64),
            "listen": (ctypes.c_uint64, ctypes.c_uint32, _CALLBACK, ctypes.c_uint64),
            "volume": (ctypes.c_char_p,),
            "cable": (ctypes.c_uint32, ctypes.c_char_p),
            "peak": (ctypes.c_char_p,),
            "capability": (ctypes.c_uint32, ctypes.c_uint64, ctypes.c_char_p),
        }.items():
            operation = getattr(dll, "lummix_media_" + name)
            operation.argtypes = types
            operation.restype = ctypes.c_void_p
        dll.lummix_media_release_session.argtypes = (ctypes.c_uint64,)
        dll.lummix_media_release_session.restype = None
        dll.lummix_media_free.argtypes = (ctypes.c_void_p,)
        dll.lummix_media_free.restype = None
        # The HTTP host can already be STA. All native calls, including finalization,
        # run away from it so reads can use a bounded MTA WinRT wait.
        self.worker = ThreadPoolExecutor(max_workers=3, thread_name_prefix="LummixRustMedia")
        self.events = {}
        self.event_ids = count(1)
        self.callback = _CALLBACK(self.deliver)

    def decode(self, function, *args):
        address = function(*args)
        if not address:
            raise OSError("Windows media returned no response")
        try:
            result = json.loads(ctypes.string_at(address))
        finally:
            self.dll.lummix_media_free(address)
        if "error" in result:
            raise OSError(result["error"])
        return result["value"]

    def call(self, op, identity=0, action=0, argument=0):
        return self.worker.submit(self.decode, self.dll.lummix_media_call,
                                  op, identity, action, argument).result()

    def volume(self, windows, expected, change):
        request = json.dumps({"windows": windows, "expected": expected, "change": change}).encode("ascii")
        return self.worker.submit(self.decode, self.dll.lummix_media_volume, request).result()

    def direct_to_cable(self, root_pid, signed_render_ids):
        endpoints = json.dumps(sorted(signed_render_ids)).encode("ascii")
        return self.worker.submit(self.decode, self.dll.lummix_media_cable, root_pid, endpoints).result()

    def peak(self, processes):
        request = json.dumps(processes).encode("ascii")
        return self.worker.submit(self.decode, self.dll.lummix_media_peak, request).result()

    def capability(self, operation, store=0, **fields):
        request = json.dumps(fields).encode("ascii")
        return self.decode(self.dll.lummix_media_capability, operation, store, request)

    def release(self, identity):
        try:
            self.worker.submit(self.dll.lummix_media_release_session, identity)
        except RuntimeError:
            pass  # Interpreter shutdown owns the DLL and the remaining sessions.

    def deliver(self, identity):
        event = self.events.get(identity)
        if event:
            event(None, None)

    def listen(self, session, kind, event):
        identity = next(self.event_ids)
        self.events[identity] = event
        try:
            token = self.worker.submit(self.decode, self.dll.lummix_media_listen,
                                       session, kind, self.callback, identity).result()
        except Exception:
            self.events.pop(identity, None)
            raise
        return identity, token

    def unlisten(self, session, kind, binding):
        identity, token = binding
        self.events.pop(identity, None)
        self.call(7, session, kind, token)


class NativeManager:
    def __init__(self, native):
        self.native = native

    def get_sessions(self):
        return [NativeSession(self.native, identity, source) for identity, source in self.native.call(0)]

    def add_sessions_changed(self, event):
        return self.native.listen(0, 0, event)

    def remove_sessions_changed(self, binding):
        self.native.unlisten(0, 0, binding)


class NativeSession:
    def __init__(self, native, identity, source):
        self.native = native
        self.identity = identity
        self.source_app_user_model_id = source
        self._release = finalize(self, native.release, identity)

    def get_playback_info(self):
        result = self.native.call(1, self.identity)
        status = result["status"]
        name = _PLAYBACK_NAMES[status] if 0 <= status < len(_PLAYBACK_NAMES) else "UNKNOWN"
        return SimpleNamespace(playback_status=SimpleNamespace(name=name),
                               controls=SimpleNamespace(**result["controls"]), playback_rate=result["playback_rate"])

    def current_timeline(self):
        return self.native.call(2, self.identity)

    def read_media(self):
        result = self.native.call(3, self.identity)
        thumbnail = result["thumbnail"]
        return tuple(result["labels"]), base64.b64decode(thumbnail) if thumbnail else None

    def matches(self, other):
        return isinstance(other, NativeSession) and self.native.call(11, self.identity, 0, other.identity)

    async def _control(self, action, position=0):
        return await asyncio.to_thread(self.native.call, 4, self.identity, action, position)

    async def try_play_async(self):
        return await self._control(0)

    async def try_pause_async(self):
        return await self._control(1)

    async def try_skip_previous_async(self):
        return await self._control(2)

    async def try_skip_next_async(self):
        return await self._control(3)

    async def try_stop_async(self):
        return await self._control(4)

    async def try_change_playback_position_async(self, position):
        return await self._control(5, position)

    def add_media_properties_changed(self, event):
        return self.native.listen(self.identity, 1, event)

    def remove_media_properties_changed(self, binding):
        self.native.unlisten(self.identity, 1, binding)

    def add_playback_info_changed(self, event):
        return self.native.listen(self.identity, 2, event)

    def remove_playback_info_changed(self, binding):
        self.native.unlisten(self.identity, 2, binding)
