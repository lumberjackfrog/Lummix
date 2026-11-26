"""Time a gain change through a running Windows duplex route and a process-loopback observer.

Both builds render a generated tone to reserved Steam test endpoints. This includes the test
control pipe, the mixer, Windows buffering, and observer scheduling. It excludes VRChat/network.
"""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import random
import subprocess
import sys
import time

import numpy as np
import psutil

import device_route

HERE = Path(__file__).resolve()
WINDOWS = HERE.parents[1]


def worker(music_pid, baseline):
    if baseline:
        sys.path.insert(0, str(baseline / "windows"))
    from cable_mixer import CableMixer
    from spotify_process_feed import SpotifyProcessFeed
    target = (music_pid, psutil.Process(music_pid).create_time(), "desktop")

    def feed(ids, **kwargs):
        if baseline:
            return SpotifyProcessFeed(ids, discover=lambda: target, guard=lambda *_: False)
        from source_fixture import synthetic_probe
        return SpotifyProcessFeed(ids, probe=synthetic_probe(lambda: target, lambda: False), **kwargs)

    mixer = CableMixer(process_feed_factory=feed)
    if baseline and mixer._stage.kernel is None:
        raise RuntimeError("Unmodified previous-build release kernel is required")
    _, mic, sink = device_route.indices()
    mixer.set_controls(mute_mic=True)
    mixer.start_mic_route(mic, sink, signed_render_ids={"reserved virtual test device"})
    try:
        print(json.dumps({"pid":os.getpid(), "latency":mixer.stream.latency, "mode":mixer.latency_mode,
                          "frames":480 if baseline else mixer.stream.frames}), flush=True)
        for line in sys.stdin:
            gain = json.loads(line)
            if gain is None:
                break
            mixer.set_controls(file_gain=gain)
            print(mixer.xruns, flush=True)
    finally:
        mixer.stop()


def start(stack, args):
    child = subprocess.Popen([sys.executable, "-Bu", *map(str, args)], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                             creationflags=subprocess.CREATE_NO_WINDOW)

    def close():
        if child.poll() is None:
            try:
                child.stdin.write("null\n")
                child.stdin.flush()
                child.wait(5)
            except (OSError, subprocess.TimeoutExpired):
                child.kill()
                child.wait(5)
        for stream in (child.stdin, child.stdout, child.stderr):
            stream.close()

    stack.callback(close)
    first = child.stdout.readline()
    if not first:
        raise RuntimeError(child.stderr.read())
    return child, first


def trial(source, baseline, count):
    from cable_mixer import Controls
    from native_audio import Engine, ProcessCapture
    with ExitStack() as stack:
        args = [HERE, "--worker", source]
        if baseline:
            args.extend(["--baseline", baseline])
        child, hello = start(stack, args)
        route = json.loads(hello)
        observer_engine = Engine(Controls())
        pid = route.pop("pid")
        observer = ProcessCapture(observer_engine, (pid, psutil.Process(pid).create_time(), "generated route"))
        stack.callback(observer.close)

        def observe(check, timeout):
            deadline = time.perf_counter() + timeout
            first = None
            while time.perf_counter() < deadline:
                packet = observer_engine.read()
                if packet is None:
                    time.sleep(.001)
                    continue
                rms = float(np.sqrt(np.mean(packet[:, 0] ** 2)))
                if check(rms):
                    if first is not None:
                        return first
                    first = time.perf_counter_ns()
                else:
                    first = None
            raise RuntimeError(f"Observer did not see the expected volume. Capture state {observer.state}")

        observe(lambda v: .034 < v < .050, 12)
        route.update(down_ms=[], up_ms=[], xruns=0)
        phases = random.Random(119)
        for i in range(count):
            down = i % 2 == 0
            time.sleep(.12 + phases.random() * .019)
            while observer_engine.read() is not None:
                pass
            stamp = time.perf_counter_ns()
            child.stdin.write("0.25\n" if down else "1.0\n")
            child.stdin.flush()
            count_line = child.stdout.readline()
            if not count_line:
                raise RuntimeError(child.stderr.read())
            route["xruns"] = int(count_line)
            seen = observe((lambda v: .006 < v < .023) if down else (lambda v: .031 < v < .052), 4)
            route["down_ms" if down else "up_ms"].append(round((seen - stamp) / 1e6, 3))
        return route


def compare(baseline, output):
    device_route.reserve()
    rows = {"before": [], "after": []}
    with ExitStack() as stack:
        _, source = start(stack, [WINDOWS / "tests/device_route.py", "--tone", "2", "787"])
        for version in ("before", "after", "after", "before"):
            rows[version].append(trial(int(source), baseline if version == "before" else None, 32))
    report = {"baseline": "c8a5a2d", "measure": "Control-pipe write to first confirmed matching output block",
              "sample_rate": 48000, "frames": 480, "runs": rows, "summary_ms": {}}
    for version, runs in rows.items():
        samples = [v for run in runs for key in ("down_ms","up_ms") for v in run[key]]
        report["summary_ms"][version] = {"n": len(samples), "median": float(np.median(samples)),
                                        "p90": float(np.percentile(samples, 90))}
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report["summary_ms"]))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", type=int)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    baseline = args.baseline.resolve() if args.baseline else None
    if args.worker:
        worker(args.worker, baseline)
    elif baseline and args.output:
        compare(baseline, args.output)
    else:
        parser.error("Provide the old checkout with --baseline and an --output JSON file")
