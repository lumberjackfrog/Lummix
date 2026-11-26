"""Windows integration test: generated audio in isolated processes, rendered to a nonphysical endpoint.

Run directly on a Windows test host with Steam Streaming Speakers installed. No app playback,
microphone or physical output is touched. The native capture targets only synthetic process trees.
"""
import os
from pathlib import Path
import subprocess
import sys
import time
import numpy as np
import psutil


def tone(frequency):
    import sounddevice as sd
    from cable_mixer import _devices
    sinks=[i for i,d in _devices() if d["name"]=="Speakers (Steam Streaming Speakers)" and d["max_output_channels"]>=2]
    if len(sinks)!=1:raise RuntimeError("The designated nonphysical test render endpoint is unavailable")
    phase=0
    def render(out,n,t,status):
        nonlocal phase
        phase_ix=np.arange(phase,phase+n)
        out[:]=(.09*np.sin(phase_ix*2*np.pi*frequency/48000))[:,None]
        phase+=n
    with sd.OutputStream(device=sinks[0],samplerate=48000,channels=2,dtype="float32",callback=render):
        time.sleep(35)


def spawn(*args):
    return subprocess.Popen([sys.executable,"-B",str(Path(__file__).resolve()),*map(str,args)],
                            creationflags=subprocess.CREATE_NO_WINDOW)


def finish(p):
    try:
        root=psutil.Process(p.pid)
        for proc in root.children(recursive=True):proc.terminate()
        root.terminate()
    except psutil.NoSuchProcess:pass
    p.wait(timeout=5)


def read(feed,seconds=.55):
    end=time.monotonic()+8;chunks=[]
    while len(chunks)<seconds*100 and time.monotonic()<end:
        chunk=feed.next()
        if chunk is None:time.sleep(.003)
        else:chunks.append(chunk)
    assert len(chunks)>=seconds*100,(feed.state,len(chunks))
    return np.concatenate(chunks)[:,0]


def powers(signal,hz):
    n=np.arange(len(signal))
    return abs(np.sum(signal*np.hanning(len(signal))*np.exp(-2j*np.pi*hz*n/48000)))*4/len(signal)


def run():
    from spotify_process_feed import SpotifyProcessFeed,supported
    assert supported()
    children=[]
    def source(hz):
        proc=spawn("--tree",hz);children.append(proc)
        return proc.pid,psutil.Process(proc.pid).create_time(),"desktop"
    target=source(525);other=spawn("--tone",1050);children.append(other)
    direct=False
    from source_fixture import synthetic_probe
    feed=SpotifyProcessFeed({"fixture"},probe=synthetic_probe(lambda:target,lambda:direct))
    try:
        data=read(feed)
        match,reject=powers(data,525),powers(data,1050)
        print("first source RMS",round(float(np.sqrt(np.mean(data**2))),4),"target",round(match,5),"unrelated",round(reject,7))
        assert match>.02 and reject<match*.02
        direct=True
        deadline=time.monotonic()+3
        while feed.state!="direct_to_cable" and time.monotonic()<deadline:time.sleep(.02)
        assert feed.next() is None and feed.state=="direct_to_cable"
        direct=False
        data=read(feed,.3)
        assert powers(data,525)>.02
        finish(children[0]);target=source(700)
        data=read(feed,.9)
        # Consume startup/old samples, then take a stable window from the new process.
        data=read(feed,.5)
        match,reject=powers(data,700),powers(data,1050)
        print("restarted source",round(match,5),"unrelated",round(reject,7),feed.state)
        assert match>.02 and reject<match*.02
        print("PASS generated child-process isolation, endpoint block/clear, recovery, process restart and release")
    finally:
        feed.close()
        for p in children:finish(p)
        assert feed.next() is None and feed.state=="off"


if __name__=="__main__":
    if len(sys.argv)>1 and sys.argv[1]=="--tone":tone(float(sys.argv[2]))
    elif len(sys.argv)>1 and sys.argv[1]=="--tree":spawn("--tone",sys.argv[2]).wait()
    else:run()
