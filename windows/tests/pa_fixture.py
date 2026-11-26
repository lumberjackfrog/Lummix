"""Deterministic PortAudio driver double. It retains and calls the real Rust callback."""
import ctypes as c
import numpy as np
import sounddevice as sd
from native_audio import PortAudio, NativeStream

ptr, integer, unsigned = c.c_void_p, c.c_int, c.c_ulong
Callback = c.CFUNCTYPE(integer, ptr, ptr, unsigned, ptr, unsigned, ptr)


class Driver:
    def __init__(self):
        self.live = self.closed = self.reject_low = False
        self.opens = []
        self.refs = []
        ffi = sd._ffi
        self.device = ffi.new("PaDeviceInfo *", dict(hostApi=1, maxInputChannels=1, maxOutputChannels=2,
            defaultLowInputLatency=.01, defaultLowOutputLatency=.01, defaultHighInputLatency=.05,
            defaultHighOutputLatency=.05))
        self.host = ffi.new("PaHostApiInfo *", dict(type=13))
        self.info = ffi.new("PaStreamInfo *", dict(inputLatency=.011,outputLatency=.014,sampleRate=48000))
        def address(p): return int(ffi.cast("uintptr_t",p))
        def bind(result, args, fn):
            cb = c.CFUNCTYPE(result,*args)(fn)
            self.refs.append(cb)
            return c.cast(cb,ptr).value
        def open_stream(place, inp, out, rate, frames, flags, callback, user):
            left = ffi.cast("PaStreamParameters *",inp)
            right = ffi.cast("PaStreamParameters *",out)
            mode = "low" if left.suggestedLatency == .01 else "high"
            self.opens.append((mode,left.device,right.device,int(frames),rate,flags))
            if self.reject_low and mode == "low":
                return -9985
            self.callback, self.user = Callback(callback),user
            place[0] = 1234
            self.closed = False
            return 0
        def start(p): self.live=True; return 0
        def abort(p): self.live=False; return 0
        def close(p): self.closed=True; return 0
        self.api = PortAudio(
            bind(ptr,[integer],lambda i:address(self.device) if i in (31,71) else 0),
            bind(ptr,[integer],lambda i:address(self.host)),
            bind(integer,[c.POINTER(ptr),ptr,ptr,c.c_double,unsigned,unsigned,ptr,ptr],open_stream),
            bind(integer,[ptr],start), bind(integer,[ptr],lambda p:int(self.live)),
            bind(ptr,[ptr],lambda p:address(self.info)),
            bind(integer,[ptr],abort),bind(integer,[ptr],close))
    def stream(self, engine, mic, sink):
        return NativeStream(engine,mic,sink,api=self.api)

    def generated(self, amplitude=.15):
        source = np.full(480, amplitude, np.float32)
        out = np.empty((480,2), np.float32)
        for _ in range(4):
            self.callback(source.ctypes.data,out.ctypes.data,480,None,0,self.user)
        return out

