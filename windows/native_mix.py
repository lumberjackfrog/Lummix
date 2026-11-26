"""The allocation-free Rust sample loop for native float32 audio blocks."""
from functools import lru_cache
from pathlib import Path
import sys

from cffi import FFI
import numpy as np


@lru_cache(maxsize=1)
def load_kernel():
    packaged = getattr(sys, "frozen", False)
    folder = Path(sys._MEIPASS) if packaged else Path(__file__).resolve().parents[1] / ".build/dsp/release"
    dll = folder / "lummix_dsp.dll"
    if not packaged and not dll.exists():
        return None
    ffi = FFI()
    ffi.cdef("""
        unsigned int lummix_dsp_abi(void);
        double lummix_mix_f32(size_t, float *, const float *, const float *,
                              const float *, float, const float *, float,
                              double, double, double, double, double);
    """)
    lib = ffi.dlopen(str(dll))
    if lib.lummix_dsp_abi() != 1:
        raise RuntimeError("The Lummix audio library has an incompatible version.")
    return Kernel(ffi, lib)


class Kernel:
    def __init__(self, ffi, library):
        self.ffi = ffi
        self.library = library
        self.samples = ffi.typeof("float[]")

    def render(self, out, mic, music, mic_gain, music_gain, duck_start, duck_end, limiter, ceiling, release):
        """Return None for layouts handled by the original strided NumPy renderer."""
        n = len(out)
        if not out.flags.writeable or not self.direct(out, (n, 2)):
            return None
        for source, channels in ((mic, 1), (music, 2)):
            if source is not None and (not self.direct(source, (n, channels)) or np.may_share_memory(out, source)):
                return None
        buffer = self.ffi.from_buffer
        null = self.ffi.NULL
        # Fader arrays are private, aligned, contiguous float32 scratch owned by MixStage.
        mic_ramp = isinstance(mic_gain, np.ndarray)
        music_ramp = isinstance(music_gain, np.ndarray)
        for ramp in (mic_gain, music_gain):
            if isinstance(ramp, np.ndarray) and (not self.direct(ramp, (n, 1)) or np.may_share_memory(out, ramp)):
                return None
        return self.library.lummix_mix_f32(
            n, buffer(self.samples, out, require_writable=True),
            null if mic is None else buffer(self.samples, mic),
            null if music is None else buffer(self.samples, music),
            buffer(self.samples, mic_gain) if mic_ramp else null, 0. if mic_ramp else mic_gain,
            buffer(self.samples, music_gain) if music_ramp else null, 0. if music_ramp else music_gain,
            duck_start, duck_end, limiter, ceiling, release)

    @staticmethod
    def direct(array, shape):
        return array.dtype == np.float32 and array.shape == shape and array.flags.c_contiguous and array.flags.aligned
