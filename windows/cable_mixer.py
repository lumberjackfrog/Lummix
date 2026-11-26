"""Lummix control plane. PortAudio calls Rust directly for every live audio buffer."""
from dataclasses import dataclass, replace
import math
from weakref import finalize

import numpy as np
import sounddevice as sd

from cable_routes import RENDERS
from native_audio import Engine, NativeStream

RATE, BLOCK, GAIN_RAMP, PEAK_CEILING = 48000, 480, 720, .98


def _devices():
    apis = sd.query_hostapis()
    return [(i, d) for i,d in enumerate(sd.query_devices()) if apis[d["hostapi"]]["name"] == "Windows WASAPI"]


@dataclass(frozen=True)
class Controls:
    mic_gain: float = 1.
    file_gain: float = 1.
    mute_mic: bool = False
    mute_file: bool = False
    mute_master: bool = False
    duck_enabled: bool = False
    duck_depth: float = 12.

    def __post_init__(self):
        if any(not math.isfinite(x) or not 0 <= x <= 2 for x in (self.mic_gain, self.file_gain)):
            raise ValueError("Gain must be a linear value from 0 to 2.")


class MixStage:
    """Offline block entry point and passive measurements for the live Rust engine."""
    def __init__(self, controls, frames=BLOCK):
        self.engine = Engine(controls)
        self._handle = self.engine.dll.lummix_stage_new(self.engine.handle)
        self._release = finalize(self, self.engine.dll.lummix_stage_free, self._handle)
        self.limit_gain = 1.

    @property
    def last_music(self):
        return bool(self.engine.metrics.music)

    @property
    def reduction_db(self):
        return self.engine.metrics.reduction

    def render(self, out, mic, music, controls):
        self.engine.configure(controls)
        n = len(out)
        if out.shape != (n, 2) or not out.flags.writeable:
            raise ValueError("Expected writable stereo output")
        target = np.require(out, np.float32, ["C","A","W"])
        def source(array, channels):
            if array is None:
                return None
            if len(array) != n or array.ndim != 2 or array.shape[1] < channels:
                raise ValueError("Invalid input buffer")
            value = np.require(array[:, :channels], np.float32, ["C","A"])
            return value.copy() if np.may_share_memory(value, target) else value
        voice, track = source(mic,1), source(music,2)
        self.limit_gain = self.engine.dll.lummix_stage_render(
            self._handle, self.engine.handle, n, target.ctypes.data,
            None if voice is None else voice.ctypes.data, None if track is None else track.ctypes.data)
        if target is not out:
            out[:] = target


def mix_into(out, mic, music, controls):
    MixStage(controls).render(out, mic, music, controls)


class CableMixer:
    def __init__(self, *, process_feed_factory=None, stream_factory=NativeStream):
        self.process_feed_factory = process_feed_factory
        self.stream_factory = stream_factory
        self.spotify_feed = self.stream = self.latency_mode = None
        self.controls = Controls()
        self._stage = MixStage(self.controls)

    def set_controls(self, **changes):
        value = replace(self.controls, **changes)
        self._stage.engine.configure(value)
        self.controls = value

    @property
    def mic_rms(self):
        return self._stage.engine.metrics.rms

    @property
    def xruns(self):
        return self._stage.engine.metrics.xruns

    def _render(self, out, mic, frames, status):
        # Offline rendering only. Live audio and meters never enter Python's interpreter.
        if frames != BLOCK:
            out.fill(0)
            return
        self._stage.render(out,mic,self.spotify_feed.next() if self.spotify_feed else None,self.controls)

    def start_mic_route(self, mic_index, sink_index, *, signed_render_ids=()):
        if self.stream is not None:
            raise RuntimeError("A stream is already open.")
        stage = MixStage(self.controls)
        stream = self.stream_factory(stage.engine, mic_index, sink_index)
        self.stream, self._stage, self.latency_mode = stream, stage, stream.mode
        if signed_render_ids:
            try:
                from spotify_process_feed import SpotifyProcessFeed
                factory = self.process_feed_factory or SpotifyProcessFeed
                self.spotify_feed = factory(signed_render_ids, engine=stage.engine)
            except Exception:
                self.stop()
                raise

    def stop(self):
        feed, self.spotify_feed = self.spotify_feed, None
        stream, self.stream = self.stream, None
        self.latency_mode = None
        try:
            if stream is not None:
                stream.close()
        finally:
            if feed is not None:
                feed.close()
