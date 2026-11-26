"""ABBA replay of process-capture packet delivery through the output callback on one Windows host.

Pass --baseline pointing to an unmodified c8a5a2d checkout with its release DSP built.
The candidate requires its normal release DLL plus a release build with --features replay
under .build/dsp-replay. Only that test binary supports packet injection.
"""
import argparse
import collections
import ctypes as ct
import json
import os
from pathlib import Path
import statistics
import subprocess
import sys
from time import perf_counter_ns, process_time_ns
from unittest.mock import patch

import numpy as np

WINDOWS = Path(__file__).resolve().parents[1]
CASES = ("normal", "ducking", "limiter")


def runner(version, baseline, case):
    if version == "before":
        sys.path.insert(0, str(baseline/"windows"))
    else:
        sys.path.insert(0, str(WINDOWS))
    import cable_mixer as mix
    if version == "before":
        from spotify_process_feed import Capture
        assert Path(mix.__file__).resolve().is_relative_to(baseline)
        feed = Capture.__new__(Capture)
        feed.accepting=True
        feed.blocks=collections.deque(maxlen=6)
        feed.tail=np.empty((0,2),np.float32)
        feed.last_signal=0
        mixer=mix.CableMixer()
        assert mixer._stage.kernel is not None
        mixer.spotify_feed=feed
        def push(packet, flags, n):
            feed.packet(None if flags & 2 else packet,n,flags)
        def render(mic,out): mixer._render(out,mic,len(out),None)
        close=lambda: None
    else:
        from pa_fixture import Driver
        import native_audio
        assert Path(mix.__file__).resolve().parent == WINDOWS
        library = native_audio.bind_library(WINDOWS.parent/".build/dsp-replay/release/lummix_dsp.dll")
        binding = patch.object(native_audio,"library",return_value=library)
        binding.start()
        ptr = ct.c_void_p
        library.lummix_replay_source.argtypes, library.lummix_replay_source.restype = [ptr],ptr
        library.lummix_replay_packet.argtypes,library.lummix_replay_packet.restype = [ptr,ptr,ct.c_size_t,ct.c_uint32],None
        library.lummix_replay_free.argtypes,library.lummix_replay_free.restype=[ptr],None
        driver=Driver()
        mixer=mix.CableMixer(stream_factory=driver.stream)
        mixer.start_mic_route(31,71)
        engine=mixer._stage.engine
        cap=library.lummix_replay_source(engine.handle)
        def push(packet,flags,n): library.lummix_replay_packet(cap,packet,n,flags)
        def render(mic,out):
            driver.callback(None if mic is None else mic.ctypes.data,out.ctypes.data,len(out),None,0,driver.user)
        def close():
            mixer.stop()
            library.lummix_replay_free(cap)
            binding.stop()
    mixer.set_controls(duck_enabled=case in ("ducking","qualification"),file_gain=2 if case=="limiter" else 1)
    return push,render,mixer.set_controls,close


def inputs(case):
    rng=np.random.default_rng(1949)
    level=30000 if case in ("limiter","qualification") else 4400
    pcm=rng.integers(-level,level,(960,2),dtype=np.int16)
    mic=rng.uniform(-.2,.2,(480,1)).astype("f4")
    return pcm,mic,np.zeros_like(mic),np.empty((480,2),"f4")


def child(args):
    push,render,controls,close=runner(args.worker,args.baseline,args.case)
    pcm,mic,quiet,out=inputs(args.case)
    tp=ct.POINTER(ct.c_int16)
    p=pcm.ctypes.data_as(tp)
    try:
        if args.case=="qualification":
            blocks=[]
            dither=np.full((480,2),-4,np.int16)
            for i in range(420):
                if i in (23,64,129,210,280,345):
                    controls(mic_gain=(i%5)/2, file_gain=2. if i<280 else .3,
                             mute_mic=i==64,mute_file=i==210,mute_master=i==345)
                if i % 59 == 3: # queue overflow, last six blocks win
                    for j in range(10):
                        pushed=pcm[j:j+480]
                        push(pushed.ctypes.data_as(tp),0,480)
                elif i%23==2: push(dither.ctypes.data_as(tp),0,480)
                elif i%17==3: push(p,0,146); push(pcm[146:].ctypes.data_as(tp),0,334)
                elif i%17==4: push(p,0,77); push(p,1,480)
                elif i%13==2: push(p,2,480)
                elif i%11!=3: push(p,0,480)
                render(quiet if 60<= i%184 <150 else mic,out)
                blocks.append(out.copy())
            np.save(args.output,np.stack(blocks),allow_pickle=False)
            return
        for _ in range(512): push(p,0,480); render(mic,out)
        groups=[]
        cpu=process_time_ns()
        wall=perf_counter_ns()
        for _ in range(args.blocks//50):
            start=perf_counter_ns()
            for i in range(50):
                push(p,0,480)
                render(quiet if args.case=="ducking" and i>=36 else mic,out)
            groups.append((perf_counter_ns()-start)/50/1e3)
        elapsed=perf_counter_ns()-wall
        cpu=process_time_ns()-cpu
        print(json.dumps(dict(us=elapsed/args.blocks/1e3,cpu_ms=cpu/1e6,
                  p95_batch_us=float(np.percentile(groups,95)),blocks=args.blocks)))
    finally: close()


def main(args):
    args.output.mkdir(parents=True,exist_ok=True)
    def run(version,case,path=None):
        cmd=[sys.executable,"-B",str(Path(__file__).resolve()),"--worker",version,"--case",case,
             "--baseline",str(args.baseline),"--output",str(path or args.output),"--blocks",str(args.blocks)]
        return subprocess.check_output(cmd,text=True)
    for version in ("before","after"):
        run(version,"qualification",args.output/(version+".npy"))
    before,after=(np.load(args.output/(v+".npy"),allow_pickle=False) for v in ("before","after"))
    error=float(np.max(np.abs(before-after)))
    np.testing.assert_allclose(before,after,rtol=0,atol=2e-7)
    results={"baseline":"c8a5a2d","channel_samples":before.size,"max_error":error,"sample_rate":48000,
             "block_frames":480,"blocks_per_pass":args.blocks,"cases":{}}
    for case in CASES:
        raw={v:[] for v in ("before","after")}
        for v in ("before","after","after","before","after","before"):
            raw[v].append(json.loads(run(v,case)))
        med={v:statistics.median(p["us"] for p in passes) for v,passes in raw.items()}
        cpu={v:statistics.median(p["cpu_ms"] for p in passes) for v,passes in raw.items()}
        results["cases"][case]=dict(before_us=med["before"],after_us=med["after"],speedup=med["before"]/med["after"],cpu_ms=cpu,runs=raw)
    (args.output/"route-benchmark.json").write_text(json.dumps(results,indent=2)+"\n",encoding="utf8")
    print(json.dumps({c:{k:round(v[k],3) for k in ("before_us","after_us","speedup")} for c,v in results["cases"].items()}))
    print(f"{before.size} samples, maximum difference {error}")


if __name__=="__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker",choices=["before","after"])
    parser.add_argument("--case")
    parser.add_argument("--baseline",required=True,type=Path)
    parser.add_argument("--output",required=True,type=Path)
    parser.add_argument("--blocks",type=int,default=15000)
    args=parser.parse_args()
    args.baseline=args.baseline.resolve()
    if args.blocks<=0 or args.blocks%50: parser.error("blocks must be a positive multiple of 50")
    (child if args.worker else main)(args)

