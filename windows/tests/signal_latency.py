"""Measure generated audio arriving at the Windows output using two process-loopback observers."""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
from queue import Empty, SimpleQueue
import random
import sys
import time

import numpy as np
import psutil
import sounddevice as sd

import device_route
import route_latency
from cable_mixer import Controls
from native_audio import Engine, ProcessCapture


HERE = Path(__file__).resolve()
LEVELS = (0.008, 0.08)


def generator():
    commands, acknowledgements = SimpleQueue(), SimpleQueue()
    level = LEVELS[0]
    # Exactly seven cycles per block. Adjacent blocks keep the carrier phase continuous.
    wave = np.sin(np.arange(480) * 2 * np.pi * 700 / 48000).astype("float32")

    def render(out, frames, timing, flags):
        nonlocal level
        try:
            level = commands.get_nowait()
            acknowledgements.put(time.perf_counter_ns())
        except Empty:
            pass
        if frames != len(wave):
            raise sd.CallbackAbort
        out[:] = wave[:, None] * level

    with sd.OutputStream(device=device_route.indices()[2], samplerate=48000, blocksize=480,
                         channels=2, dtype="float32", callback=render):
        print(os.getpid(), flush=True)
        for line in sys.stdin:
            requested = json.loads(line)
            if requested is None:
                return
            commands.put(requested)
            print(acknowledgements.get(timeout=3), flush=True)


def tap(stack, pid):
    engine = Engine(Controls())
    capture = ProcessCapture(engine, (pid, psutil.Process(pid).create_time(), "signal timing fixture"))
    stack.callback(capture.close)
    return engine, capture


def arrival(taps, high, timeout):
    pending = dict(taps)
    first = {}
    deadline = time.perf_counter() + timeout
    lo, hi = (.040, .078) if high else (.003, .012)
    while pending and time.perf_counter() < deadline:
        for name, (engine, capture) in list(pending.items()):
            if capture.state < 0:
                raise RuntimeError(f"{name} loopback failed: {capture.state}")
            for _ in range(8):
                packet = engine.read()
                if packet is None:
                    break
                stamp = time.perf_counter_ns()
                rms = float(np.sqrt(np.mean(packet[:, 0] ** 2)))
                if lo < rms < hi:
                    if name in first:
                        del pending[name]
                        break
                    first[name] = stamp
                else:
                    first.pop(name, None)
        if pending:
            time.sleep(.001)
    if pending:
        raise TimeoutError(f"Two consecutive blocks at the expected level did not reach {list(pending)}")
    return first


def measure(player, player_pid, baseline, edges):
    with ExitStack() as stack:
        cmd = [HERE.with_name("route_latency.py"), "--worker", player_pid]
        if baseline:
            cmd.extend(["--baseline", baseline])
        router, hello = route_latency.start(stack, cmd)
        route = json.loads(hello)
        taps = {"source": tap(stack, player_pid), "output": tap(stack, route["pid"])}
        arrival(taps, False, 12)
        phases = random.Random(462)
        samples = []
        for index in range(edges):
            time.sleep(.13 + phases.random() * .06)
            for engine, _ in taps.values():
                while engine.read() is not None:
                    pass
            high = index % 2 == 0
            player.stdin.write(f"{LEVELS[int(high)]}\n")
            player.stdin.flush()
            submitted = int(player.stdout.readline())
            stamps = arrival(taps, high, 3)
            samples.append({
                "source_ms": (stamps["source"] - submitted) / 1e6,
                "output_ms": (stamps["output"] - submitted) / 1e6,
                "between_taps_ms": (stamps["output"] - stamps["source"]) / 1e6,
            })
        router.stdin.write("1.0\n")
        router.stdin.flush()
        return {"frames": route["frames"], "xruns": int(router.stdout.readline()), "edges": samples}


def compare(baseline, output, order, edges):
    device_route.reserve()
    runs = {"before": [], "rust": []}
    with ExitStack() as stack:
        player, message = route_latency.start(stack, [HERE, "--generator"])
        for version in order:
            key = "before" if version == "A" else "rust"
            runs[key].append(measure(player, int(message), baseline if version == "A" else None, edges))

    summary = {}
    for name, trials in runs.items():
        samples = [edge for trial in trials for edge in trial["edges"]]
        totals = {"edges": len(samples), "xruns": sum(trial["xruns"] for trial in trials)}
        for key in ("source_ms", "output_ms", "between_taps_ms"):
            median, p90 = np.percentile([edge[key] for edge in samples], [50, 90])
            totals[key] = {"median": round(float(median), 3), "p90": round(float(p90), 3)}
        summary[name] = totals
    report = {"order": order, "sample_rate": 48000, "summary": summary, "runs": runs}
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--generator", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--order", choices=["ABBA", "BAAB"], default="ABBA")
    parser.add_argument("--edges", type=int, default=32)
    options = parser.parse_args()
    if options.generator:
        generator()
    elif options.baseline and options.output and options.edges >= 2 and options.edges % 2 == 0:
        compare(options.baseline.resolve(strict=True), options.output, options.order, options.edges)
    else:
        parser.error("Provide --baseline, --output and an even --edges of at least two.")
