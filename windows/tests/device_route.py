"""Exercise the real duplex callback with dedicated Steam virtual audio endpoints.

This opt-in bench exits if the named endpoints are missing, active, or system defaults.
It does not open a physical microphone, the signed game cable, or a real media player.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time

import numpy as np
import psutil
import sounddevice as sd

NAMES=("Speakers (Steam Streaming Microphone)","Microphone (Steam Streaming Microphone)",
       "Speakers (Steam Streaming Speakers)")
WINDOWS=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(WINDOWS))


def indices():
    from cable_mixer import _devices
    rows=_devices()
    result=[]
    for name in NAMES:
        found=[i for i,d in rows if d["name"]==name]
        if len(found)!=1: raise RuntimeError("Dedicated test endpoint unavailable: "+name)
        result.append(found[0])
    return result


def reserve():
    import comtypes
    from pycaw.api.audiopolicy import IAudioSessionManager2
    from pycaw.pycaw import AudioUtilities as A
    e=A.GetDeviceEnumerator()
    for flow in (0,1):
        for role in (0,1,2):
            if A.CreateDevice(e.GetDefaultAudioEndpoint(flow,role)).FriendlyName in NAMES:
                raise RuntimeError("A test device is in use as a system default")
    for flow in (0,1):
        devices=e.EnumAudioEndpoints(flow,1)
        for i in range(devices.GetCount()):
            device=devices.Item(i)
            if A.CreateDevice(device).FriendlyName not in NAMES: continue
            manager=device.Activate(IAudioSessionManager2._iid_,comtypes.CLSCTX_ALL,None)
            sessions=manager.QueryInterface(IAudioSessionManager2).GetSessionEnumerator()
            if any(sessions.GetSession(j).GetState()==1 for j in range(sessions.GetCount())):
                raise RuntimeError("A dedicated test endpoint already has an active session")


def tone(which,frequency):
    phase=0
    def render(out,n,t,flags):
        nonlocal phase
        out[:]=(.06*np.sin((phase+np.arange(n))*2*np.pi*frequency/48000))[:,None]
        phase+=n
    with sd.OutputStream(device=indices()[which],samplerate=48000,blocksize=480,channels=2,dtype="float32",callback=render):
        print("ready",flush=True)
        sys.stdin.readline()


def route(pid):
    from cable_mixer import CableMixer
    from spotify_process_feed import SpotifyProcessFeed
    target=(pid,psutil.Process(pid).create_time(),"generated")
    def feed(ids, **kwargs):
        from source_fixture import synthetic_probe
        return SpotifyProcessFeed(ids,probe=synthetic_probe(lambda:target,lambda:False),**kwargs)
    mixer=CableMixer(process_feed_factory=feed)
    _,mic,sink=indices()
    mixer.start_mic_route(mic,sink,signed_render_ids={"isolated test"})
    try:
        print(json.dumps(dict(active=mixer.stream.active,latency=mixer.stream.latency,mode=mixer.latency_mode)),flush=True)
        for line in sys.stdin:
            changes=json.loads(line)
            if changes is None: break
            mixer.set_controls(**changes)
            print(json.dumps(dict(xruns=mixer.xruns,rms=mixer.mic_rms,reduction=mixer._stage.reduction_db)),flush=True)
    finally:
        mixer.stop()


def run():
    from native_audio import Engine,ProcessCapture
    from cable_mixer import Controls
    reserve()
    children=[]
    observer=None
    def child(*args):
        p=subprocess.Popen([sys.executable,"-Bu",__file__,*map(str,args)],stdin=subprocess.PIPE,
          stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,creationflags=subprocess.CREATE_NO_WINDOW)
        children.append(p)
        msg=p.stdout.readline()
        if not msg: raise RuntimeError(p.stderr.read())
        return p,msg
    def send(p, obj):
        p.stdin.write(json.dumps(obj)+"\n");p.stdin.flush()
        return json.loads(p.stdout.readline())
    try:
        child("--tone","0","491")
        # This virtual driver can resample its paired mic. Calibrate against the actual
        # samples Windows delivers to a mono shared-mode client, before starting the mixer.
        with sd.InputStream(device=indices()[1], channels=1, samplerate=48000, blocksize=480,
                            dtype="float32",extra_settings=sd.WasapiSettings(auto_convert=True)) as source:
            source.read(12000)
            reference=source.read(24000)[0][:,0]
        freq=np.fft.rfftfreq(len(reference),1/48000)
        ref_hz=float(freq[np.argmax(abs(np.fft.rfft(reference*np.hanning(len(reference)))) )])
        if not (185<ref_hz<290 or 410<ref_hz<550): raise RuntimeError("Unrecognized virtual microphone reference")
        print("Windows virtual microphone reference",ref_hz,"Hz",flush=True)
        music,_=child("--tone","2","787")
        router,info=child("--route",music.pid)
        print("real WASAPI route:",info.strip(),flush=True)
        engine=Engine(Controls())
        observer=ProcessCapture(engine,(router.pid,psutil.Process(router.pid).create_time(),"route under test"))
        def measure(label, changes):
            status=send(router,changes)
            time.sleep(.45)
            for _ in range(6):engine.read()
            parts=[];end=time.monotonic()+5
            while len(parts)<50 and time.monotonic()<end:
                if observer.state<0:raise RuntimeError(f"Capture failed {observer.state}")
                samples=engine.read()
                if samples is not None: parts.append(samples[:,0])
                else:time.sleep(.002)
            if len(parts)<50: raise RuntimeError("Did not receive the isolated route output: "+label)
            audio=np.concatenate(parts)
            # A virtual device has its own clock. Measure band energy over short windows
            # so a little clock drift cannot cancel a real tone over half a second.
            windows=audio.reshape(-1,2400)
            hann=np.hanning(2400)
            spectrum=np.mean(abs(np.fft.rfft(windows*hann,axis=1))**2,axis=0)
            freq=np.fft.rfftfreq(2400,1/48000)
            levels=[float(2*np.sqrt(np.sum(spectrum[abs(freq-hz)<90])/(2400*np.sum(hann*hann))))
                    for hz in (ref_hz,787)]
            print(label,levels,status,flush=True)
            return levels
        both=measure("voice and music",{})
        muted=measure("mic muted",{"mute_mic":True})
        clean=measure("music muted",{"mute_mic":False,"mute_file":True})
        if not (min(both)>.012 and muted[0]<.002 and muted[1]>.012 and clean[0]>.012 and clean[1]<.002):
            raise AssertionError("Independent tones or mutes were not observed at the virtual output")
        doubled=measure("200 percent voice",{"mic_gain":2})
        ducked=measure("12 dB ducking",{"mic_gain":1,"mute_file":False,"duck_enabled":True,"duck_depth":12})
        np.testing.assert_allclose(doubled[0]/clean[0],2,rtol=.08)
        np.testing.assert_allclose(ducked[1]/both[1],10**(-12/20),rtol=.13)
        print("PASS dedicated virtual capture, duplex routing, two independent mutes, 200% gain, 12 dB duck.",flush=True)
    finally:
        if observer:observer.close()
        for p in reversed(children):
            if p.poll() is None:
                try:
                    p.stdin.write("null\n");p.stdin.flush();p.wait(4)
                except (OSError,subprocess.TimeoutExpired):
                    p.terminate();p.wait(3)
            for f in (p.stdin,p.stdout,p.stderr): f.close()


if __name__=="__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tone",nargs=2)
    parser.add_argument("--route",type=int)
    args=parser.parse_args()
    if args.tone:tone(int(args.tone[0]),float(args.tone[1]))
    elif args.route:route(args.route)
    else:run()

