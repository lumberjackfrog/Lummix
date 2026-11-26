"""Compare both renderers using generated PCM only. No audio endpoints are opened."""
from hashlib import sha256
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
from time import perf_counter_ns, process_time_ns
import unittest
from unittest.mock import patch

import numpy as np
import psutil

WINDOWS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(WINDOWS))
import cable_mixer as current


def original_module():
    path = os.environ.get("LUMMIX_QA_ORIGINAL")
    if not path:
        return current
    spec = importlib.util.spec_from_file_location("reference_cable_mixer", path)
    original = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = original
    spec.loader.exec_module(original)
    return original


ORIGINAL = original_module()
CASES = ("impulse", "normal", "limiter", "talkover", "faders", "nonfinite")


def mixer(native):
    module = current if native else ORIGINAL
    if not native and module is current:
        with patch.object(current, "load_kernel", return_value=None):
            return current.CableMixer()
    result = module.CableMixer()
    if native and result._stage.kernel is None:
        raise RuntimeError("The Rust DSP must be built before running this test.")
    return result


def signals(case, frames):
    idx = np.arange(frames)
    mic = np.where(idx % 31 < 15, .046875, -.09375).astype("f4")[:, None]
    music = np.column_stack((np.where(idx % 13 < 6, .21875, -.3125),
                             np.where(idx % 17 < 9, -.15625, .25))).astype("f4")
    if case == "impulse":
        mic.fill(0)
        music.fill(0)
        mic[0] = .25
        music[-1] = [.5, -.25]
    elif case == "limiter":
        music[::13] = [1.875, -.625]
    elif case == "nonfinite":
        music[::11, 0] = np.nan
        music[::23, 1] = -np.inf
        mic[-1] = np.inf
    return mic, music


def controls(case, index):
    if case == "talkover":
        return current.Controls(duck_enabled=True, duck_depth=18 if index < 180 else 7)
    if case != "faders":
        return current.Controls()
    return current.Controls(mic_gain=(0, .5, 1.5, 2)[index % 4],
                            file_gain=(2, .25, 0, 1)[index % 4],
                            mute_mic=index % 8 == 4, mute_file=index % 9 == 2,
                            mute_master=index % 15 == 11)


def sequence(case, stage):
    frames = current.BLOCK
    mic, music = signals(case, frames)
    quiet = np.zeros_like(mic)
    out = np.empty_like(music)
    for index in range(260 if case == "talkover" else 32):
        voice = quiet if case == "talkover" and 36 <= index % 210 < 185 else mic
        if case == "faders":
            voice = None if index % 13 == 6 else voice
            track = None if index % 7 == 3 else music
        else:
            track = music
        stage.render(out, voice, track, controls(case, index))
        yield out, stage.limit_gain, stage.duck.amount


def fingerprint(case, native):
    digest = sha256()
    for out, _, _ in sequence(case, mixer(native)._stage):
        digest.update(np.rint(out * 32767).astype("<i2").tobytes())
    return digest.hexdigest()


class Equivalence(unittest.TestCase):
    def test_continuous_output_and_state(self):
        sample_count, worst = 0, 0.
        native_blocks, muted_blocks = 0, 0
        for case in CASES:
            reference, migrated = mixer(False)._stage, mixer(True)._stage
            render = migrated.kernel.render
            def require_native(*args):
                nonlocal native_blocks
                result = render(*args)
                self.assertIsNotNone(result, "contiguous production audio fell back")
                native_blocks += 1
                return result
            with self.subTest(case=case), patch.object(migrated.kernel, "render", require_native):
                trace = zip(sequence(case, reference), sequence(case, migrated), strict=True)
                for index, (old, new) in enumerate(trace):
                    diff = float(np.abs(new[0] - old[0]).max())
                    worst = max(worst, diff)
                    self.assertLessEqual(diff, 2e-7)
                    self.assertAlmostEqual(old[1], new[1], places=11)
                    self.assertEqual(old[2], new[2])
                    self.assertTrue(np.isfinite(new[0]).all())
                    sample_count += new[0].size
                    muted_blocks += int(controls(case, index).mute_master)
                    self.assertEqual((native_blocks + muted_blocks) * current.BLOCK * 2, sample_count)
        print(f"Compared {sample_count} float32 channel samples; maximum absolute error {worst:.9g}")
        print(f"Rust rendered {native_blocks} contiguous blocks; the master mute silenced {muted_blocks} blocks directly.")

    def test_short_blocks_and_layout_changes(self):
        rng = np.random.default_rng(21)
        ref, rust = mixer(False)._stage, mixer(True)._stage
        for n in (1, 17, 256, 479, 480, 3, 41):
            for layout in ("native", "strided", "mono music", "alias", "native"):
                mic = rng.uniform(-.7, .7, (n, 1)).astype("f4")
                music = rng.uniform(-2, 2, (n, 2)).astype("f4")
                src = music[:, :1] if layout == "mono music" else music
                outs = [np.empty((n, 2), "f4") for _ in range(2)]
                if layout == "strided":
                    outs = [np.empty((n, 4), "f4")[:, ::2] for _ in range(2)]
                    mic = mic[::-1]
                for stage, out in zip((ref, rust), outs):
                    if layout == "alias":
                        out[:] = src
                    stage.render(out, mic, out if layout == "alias" else src, current.Controls(file_gain=1.5))
                np.testing.assert_allclose(*outs, rtol=0, atol=2e-7)
        self.assertAlmostEqual(ref.limit_gain, rust.limit_gain, places=11)

    def test_no_added_frames(self):
        stage = mixer(True)._stage
        out = next(sequence("impulse", stage))[0]
        np.testing.assert_array_equal(out[0], [.25, .25])
        np.testing.assert_array_equal(out[-1], [.5, -.25])
        self.assertFalse(out[1:-1].any())


def sustained_benchmark(native):
    """Time the callback over ten minutes of generated audio, run faster than real time."""
    engine = mixer(native)
    mic, normal = signals("normal", current.BLOCK)
    _, hot = signals("limiter", current.BLOCK)
    quiet = np.zeros_like(mic)
    out = np.empty_like(normal)
    default = current.Controls()
    talkover = current.Controls(duck_enabled=True, duck_depth=12)
    boosted = current.Controls(mic_gain=1.5, file_gain=1.5)
    schedule = ([(mic, normal, default)] * 200 + [(mic, hot, boosted)] * 100 +
                [(mic, normal, talkover)] * 120 + [(quiet, normal, talkover)] * 180)

    class Feed:
        def next(self):
            return self.track

    feed = engine.spotify_feed = Feed()

    def render_cycle():
        for voice, track, settings in schedule:
            feed.track = track
            engine.controls = settings
            engine._render(out, voice, current.BLOCK, None)

    process = psutil.Process()
    memory_field = "private" if sys.platform == "win32" else "rss"
    render_cycle()
    memory = [getattr(process.memory_info(), memory_field)]
    digest = sha256()
    cpu_ns = 0
    for _ in range(10):
        start = process_time_ns()
        for _ in range(10):
            render_cycle()
        cpu_ns += process_time_ns() - start
        memory.append(getattr(process.memory_info(), memory_field))
        digest.update(np.rint(out * 32767).astype("<i2").tobytes())
    blocks = len(schedule) * 100
    audio_seconds = blocks * current.BLOCK / current.RATE
    return {"blocks": blocks, "audioSeconds": audio_seconds, "processCpuSeconds": cpu_ns / 1e9,
            "cpuMsPerAudioSecond": cpu_ns / 1e6 / audio_seconds, "memoryMetric": memory_field,
            "memoryBytesAfterWarmupAndEachMinute": memory, "endBlockDigest": digest.hexdigest()}


def benchmark():
    timings = {}
    for case in ("normal", "limiter", "talkover"):
        mic, track = signals(case, current.BLOCK)
        quiet = np.zeros_like(mic)
        out = np.empty_like(track)
        class Feed:
            def next(self):
                return track
        records = {"numpy": [], "rust": []}
        for repeat in range(4):
            for native in ((False, True) if repeat % 2 == 0 else (True, False)):
                engine = mixer(native)
                engine.spotify_feed = Feed()
                for index in range(1536):
                    engine.controls = controls(case, index % 256)
                    voice = quiet if case == "talkover" and index % 160 > 60 else mic
                    start = perf_counter_ns()
                    engine._render(out, voice, current.BLOCK, None)
                    elapsed = perf_counter_ns() - start
                    if index >= 128:
                        records["rust" if native else "numpy"].append(elapsed / 1000)
        timings[case] = {name: dict(zip(("p50_us", "p95_us", "p99_us"), np.percentile(values, [50, 95, 99])))
                         for name, values in records.items()}
    report = {"frames": current.BLOCK, "rate": current.RATE, "blocksPerBackendPerCase": len(records["rust"]),
              "timed": "CableMixer._render with generated audio, including Python/FFI overhead", "callbacks": timings}
    # Separate processes keep one renderer's allocations out of the other one's memory readings.
    sustained = {name: json.loads(subprocess.check_output(
        [sys.executable, "-B", __file__, "sustained", name], text=True)) for name in ("numpy", "rust")}
    if sustained["numpy"]["endBlockDigest"] != sustained["rust"]["endBlockDigest"]:
        raise RuntimeError("The sustained mixers produced different end-of-minute audio samples.")
    report["sustained"] = sustained
    print(json.dumps(report, indent=2))
    return report


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "fingerprint":
        if len(sys.argv) != 4 or sys.argv[2] not in ("numpy", "rust") or sys.argv[3] not in CASES:
            print("Choose one of: " + ", ".join(CASES))
            sys.exit(2)
        print(fingerprint(sys.argv[3], sys.argv[2] == "rust"))
    elif len(sys.argv) == 3 and sys.argv[1] == "sustained" and sys.argv[2] in ("numpy", "rust"):
        print(json.dumps(sustained_benchmark(sys.argv[2] == "rust")))
    elif len(sys.argv) == 3 and sys.argv[1] == "benchmark":
        Path(sys.argv[2]).write_text(json.dumps(benchmark(), indent=2) + "\n", encoding="utf-8")
    else:
        unittest.main()
