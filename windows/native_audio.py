"""Control-plane bindings for the Rust WASAPI capture and duplex audio engine."""
import ctypes as ct
from functools import lru_cache
import json
from pathlib import Path
import sys
import threading
from weakref import finalize

import numpy as np
import sounddevice as sd


class Settings(ct.Structure):
    _fields_ = [("mic", ct.c_double), ("music", ct.c_double), ("depth", ct.c_double), ("flags", ct.c_uint32)]

    @classmethod
    def from_controls(cls, c):
        flags = c.mute_mic | (c.mute_file << 1) | (c.mute_master << 2) | (c.duck_enabled << 3)
        return cls(c.mic_gain, c.file_gain, c.duck_depth, flags)


class Metrics(ct.Structure):
    _fields_ = [("rms", ct.c_float), ("reduction", ct.c_float), ("music", ct.c_uint32), ("xruns", ct.c_uint64)]


class PortAudio(ct.Structure):
    _fields_ = [(n, ct.c_void_p) for n in ("Pa_GetDeviceInfo", "Pa_GetHostApiInfo", "Pa_OpenStream", "Pa_StartStream",
                                             "Pa_IsStreamActive", "Pa_GetStreamInfo", "Pa_AbortStream", "Pa_CloseStream")]

    @classmethod
    def loaded(cls):
        return cls(*(int(sd._ffi.cast("uintptr_t", getattr(sd._lib, n))) for n, _ in cls._fields_))


class OpenResult(ct.Structure):
    _fields_ = [("handle", ct.c_void_p), ("error", ct.c_int), ("high", ct.c_int)]


class Latency(ct.Structure):
    _fields_ = [("input", ct.c_double), ("output", ct.c_double)]


class Pair(ct.Structure):
    _fields_ = [("mic", ct.c_int), ("sink", ct.c_int)]


@lru_cache(maxsize=1)
def library():
    folder = Path(sys._MEIPASS) if getattr(sys, "frozen", False) else Path(__file__).resolve().parents[1]/".build/dsp/release"
    return bind_library(folder / "lummix_dsp.dll")


def bind_library(path):
    dll = ct.CDLL(str(path))
    ptr = ct.c_void_p
    def bind(name, result, *args):
        fn = getattr(dll, "lummix_" + name)
        fn.argtypes, fn.restype = args, result
    bind("engine_new", ptr, Settings)
    bind("engine_free", None, ptr)
    bind("engine_controls", ct.c_bool, ptr, Settings)
    bind("engine_metrics", Metrics, ptr)
    bind("engine_read", ct.c_bool, ptr, ptr)
    bind("stage_new", ptr, ptr)
    bind("stage_free", None, ptr)
    bind("stage_render", ct.c_double, ptr, ptr, ct.c_size_t, ptr, ptr, ptr)
    bind("capture_open", ptr, ptr, ct.c_uint32, ct.c_double)
    bind("capture_state", ct.c_int, ptr)
    bind("capture_close", None, ptr)
    bind("route_open", OpenResult, ptr, ct.POINTER(PortAudio), ct.c_int, ct.c_int, ptr)
    bind("route_close", ct.c_int, ptr)
    bind("route_active", ct.c_int, ptr)
    bind("route_latency", Latency, ptr)
    bind("inputs", ptr)
    bind("inputs_free", None, ptr)
    bind("route_indexes", Pair, ct.POINTER(PortAudio), ptr, ptr, ptr, ct.c_char_p, ct.c_char_p)
    return dll


def endpoint_inventory():
    dll = library()
    value = dll.lummix_inputs()
    if not value:
        raise OSError("Windows microphone inventory unavailable")
    try:
        return json.loads(ct.string_at(value))
    finally:
        dll.lummix_inputs_free(value)


def device_pair(input_id, sinks):
    pa = ct.CDLL(sd._libname)
    def address(name):
        return ct.cast(getattr(pa, name, None), ct.c_void_p).value
    result = library().lummix_route_indexes(ct.byref(PortAudio.loaded()), address("Pa_GetDeviceCount"),
                    address("PaWasapi_GetIMMDevice"), address("PaWasapi_UpdateDeviceList"),
                    input_id.encode("utf8"), json.dumps(sinks).encode("utf8"))
    return result.mic, result.sink


class Engine:
    def __init__(self, settings):
        self.dll = library()
        self.handle = self.dll.lummix_engine_new(Settings.from_controls(settings))
        if not self.handle:
            raise ValueError("Invalid mixer settings")
        self._release = finalize(self, self.dll.lummix_engine_free, self.handle)
        self.settings = settings

    def configure(self, settings):
        if settings != self.settings:
            if not self.dll.lummix_engine_controls(self.handle, Settings.from_controls(settings)):
                raise ValueError("Invalid mixer settings")
            self.settings = settings

    @property
    def metrics(self):
        return self.dll.lummix_engine_metrics(self.handle)

    def read(self):
        out = np.empty((480, 2), np.float32)
        return out if self.dll.lummix_engine_read(self.handle, out.ctypes.data) else None


class NativeStream:
    def __init__(self, engine, mic, sink, *, api=None):
        self.engine = engine
        self.api = PortAudio.loaded() if api is None else api
        self.options = sd.WasapiSettings(auto_convert=True)
        settings = int(sd._ffi.cast("uintptr_t", self.options._streaminfo))
        result = engine.dll.lummix_route_open(engine.handle, ct.byref(self.api), mic, sink, settings)
        if not result.handle:
            raise sd.PortAudioError("Could not open the selected Lummix WASAPI route", result.error)
        self.handle = result.handle
        self.mode = "high" if result.high else "low"
        self.lock = threading.Lock()

    @property
    def active(self):
        with self.lock:
            return bool(self.handle and self.engine.dll.lummix_route_active(self.handle) == 1)

    @property
    def latency(self):
        with self.lock:
            if not self.handle:
                return (0., 0.)
            result = self.engine.dll.lummix_route_latency(self.handle)
            return result.input, result.output

    def close(self):
        with self.lock:
            if self.handle:
                handle, self.handle = self.handle, None
                err = self.engine.dll.lummix_route_close(handle)
                if err:
                    raise sd.PortAudioError("Could not close the Lummix route", err)


class ProcessCapture:
    def __init__(self, engine, target):
        pid, created, self.edition = target
        self.target, self.engine = target, engine
        self.handle = engine.dll.lummix_capture_open(engine.handle, pid, created)
        if not self.handle:
            raise OSError("Spotify capture target unavailable or restarted")
        self.lock = threading.Lock()

    @property
    def state(self):
        with self.lock:
            return self.engine.dll.lummix_capture_state(self.handle) if self.handle else 3

    def close(self):
        with self.lock:
            if self.handle:
                self.engine.dll.lummix_capture_close(self.handle)
                self.handle = None

