"""Opt-in, user-mode local mic and Spotify mixer. Importing never opens a stream."""
from dataclasses import dataclass, replace
import math

import numpy as np
import sounddevice as sd
from duck_envelope import Envelope

RATE = 48000
BLOCK = 480
GAIN_RAMP = round(.015 * RATE)
LIMITER_RELEASE = round(.080 * RATE)
PEAK_CEILING = .98
from cable_routes import RENDERS


def _devices():
    apis = sd.query_hostapis()
    return [(i, d) for i, d in enumerate(sd.query_devices()) if apis[d["hostapi"]]["name"] == "Windows WASAPI"]


@dataclass(frozen=True)
class Controls:
    mic_gain: float = 1.0
    file_gain: float = 1.0
    mute_mic: bool = False
    mute_file: bool = False
    mute_master: bool = False
    duck_enabled: bool = False
    duck_depth: float = 12.0

    def __post_init__(self):
        if any(not math.isfinite(x) or not 0 <= x <= 2 for x in [self.mic_gain, self.file_gain]):
            raise ValueError("Gain must be a linear value from 0 to 2.")


class _Fader:
    def __init__(self, initial, frames):
        self.current = self.target = initial
        self.remaining = 0
        self.steps = np.arange(1, frames + 1, dtype=np.float64)
        self.gains = np.empty((frames, 1), dtype=np.float32)

    def gain(self, target, muted, frames):
        if muted:
            # Mute is immediate. Unmute fades up from silence.
            self.current = self.target = 0.
            self.remaining = 0
            return 0.
        if target != self.target:
            self.target = target
            self.remaining = GAIN_RAMP
        if not self.remaining:
            return target
        count = min(frames, self.remaining)
        step = (target - self.current) / self.remaining
        self.gains[:count, 0] = self.current + self.steps[:count] * step
        self.current += count * step
        self.remaining -= count
        if not self.remaining:
            self.current = target
            self.gains[count:frames, 0] = target
        return self.gains[:frames]


class MixStage:
    """State belongs to one 48 kHz audio stream. Callback has no locks or I/O."""
    def __init__(self, controls, frames=BLOCK):
        self.mic = _Fader(controls.mic_gain, frames)
        self.file = _Fader(controls.file_gain, frames)
        self.scratch = np.empty((frames, 2), dtype=np.float32)
        self.peaks = np.empty(frames, dtype=np.float64)
        self.release_steps = np.arange(1, frames+1, dtype=np.float64) / LIMITER_RELEASE
        self.limit_gain = 1.
        self.duck = Envelope()
        self.clock = 0.
        self.duck_gain = 1.
        self.interp = np.arange(1, frames + 1, dtype=np.float32)
        self.attenuation = np.empty(frames, dtype=np.float32)
        self.last_music = False

    def render(self, out, mic, music, controls, *, mic_rms=None):
        n = len(out)
        mic_muted = controls.mute_master or controls.mute_mic
        file_muted = controls.mute_master or controls.mute_file
        mic_gain = self.mic.gain(controls.mic_gain, mic_muted, n)
        file_gain = self.file.gain(controls.file_gain, file_muted, n)
        self.clock += n / RATE
        self.last_music = music is not None
        detect = (controls.duck_enabled and not mic_muted and not file_muted and
                  controls.mic_gain > 0 and mic is not None and self.last_music)
        if detect and mic_rms is None:
            mic_rms = math.sqrt(float(np.vdot(mic, mic)) / mic.size)
        reduction = self.duck.step(self.clock, mic_rms if detect else 0.,
                                   self.clock if detect else None, controls.duck_depth if detect else 0.)
        previous_gain = self.duck_gain
        self.duck_gain = 10 ** (-reduction / 20)
        out.fill(0)
        if controls.mute_master:
            self.limit_gain = 1.
            return
        if mic is not None and not mic_muted:
            np.multiply(mic[:, :1], mic_gain, out=out)
        scratch = self.scratch[:n]
        if music is not None and not file_muted:
            np.multiply(music, file_gain, out=scratch)
            if self.duck_gain != previous_gain:
                envelope = self.attenuation[:n]
                np.multiply(self.interp[:n], (self.duck_gain - previous_gain) / n, out=envelope)
                envelope += previous_gain
                np.multiply(scratch, envelope[:, None], out=scratch)
            elif self.duck_gain != 1.:
                scratch *= self.duck_gain
            np.add(out, scratch, out=out)
        np.nan_to_num(out, copy=False, nan=0., posinf=0., neginf=0.)
        # Linked stereo sample-peak limiter. One gain preserves the stereo image.
        # Instant attack protects the first hot sample without delaying the microphone.
        peaks = self.peaks[:n]
        np.abs(out, out=scratch)
        np.maximum(scratch[:, 0], scratch[:, 1], out=peaks)
        if self.limit_gain == 1. and peaks.max() <= PEAK_CEILING:
            return
        np.maximum(peaks, PEAK_CEILING, out=peaks)
        np.divide(PEAK_CEILING, peaks, out=peaks)
        # g[i] = min(peak_safe_gain[i], g[i-1] + release_step), vectorized.
        steps = self.release_steps[:n]
        peaks -= steps
        np.minimum(peaks, self.limit_gain, out=peaks)
        np.minimum.accumulate(peaks, out=peaks)
        peaks += steps
        self.limit_gain = float(peaks[-1])
        np.multiply(out, peaks[:, None], out=out)
        np.clip(out, -PEAK_CEILING, PEAK_CEILING, out=out)


def mix_into(out, mic, music, controls):
    """Mix an independent buffer at fixed gains. Continuous audio uses one MixStage."""
    if len(out):
        MixStage(controls, len(out)).render(out, mic, music, controls)


class CableMixer:
    """GUI calls are serialized by its controller. start() requires a user's action."""
    def __init__(self, *, process_feed_factory=None):
        self.process_feed_factory=process_feed_factory
        self.spotify_feed=None
        self.controls=Controls()
        self._stage=MixStage(self.controls)
        self.stream=None
        self.xruns=0
        self.mic_rms=0.0
        self.latency_mode=None

    def set_controls(self,**changes):
        self.controls=replace(self.controls,**changes)

    def _render(self,out,mic,frames,status):
        if status:
            self.xruns+=1
        if frames!=BLOCK:
            out.fill(0)
            return
        self.mic_rms = math.sqrt(float(np.vdot(mic, mic)) / mic.size) if mic is not None else 0.0
        feed=self.spotify_feed
        self._stage.render(out,mic,feed.next() if feed else None,self.controls,mic_rms=self.mic_rms)

    def start_mic_route(self, mic_index, sink_index, *, signed_render_ids=()):
        """Explicit, shared-mode physical mic to qualified cable; never open default devices."""
        if self.stream is not None:
            raise RuntimeError("A stream is already open.")
        self._stage = MixStage(self.controls)
        options = dict(device=(mic_index, sink_index), channels=(1, 2), samplerate=RATE,
                       blocksize=BLOCK, dtype="float32", extra_settings=sd.WasapiSettings(auto_convert=True),
                       callback=lambda inp,out,n,t,status: self._render(out,inp,n,status))
        for latency in ("low", "high"):
            stream = None
            try:
                stream = sd.Stream(latency=latency, **options)
                stream.start()
            except sd.PortAudioError:
                if stream is not None:
                    stream.close()
                if latency == "high":
                    raise
            else:
                self.stream, self.latency_mode = stream, latency
                if signed_render_ids:
                    try:
                        from spotify_process_feed import SpotifyProcessFeed
                        factory=self.process_feed_factory or SpotifyProcessFeed
                        self.spotify_feed=factory(signed_render_ids)
                    except Exception:
                        self.stop()
                        raise
                return

    def stop(self):
        feed,self.spotify_feed=self.spotify_feed,None
        stream,self.stream=self.stream,None
        self.latency_mode=None
        self.mic_rms=0.0
        try:
            if stream:
                try:
                    stream.abort()
                finally:
                    stream.close()
        finally:
            if feed:feed.close()
