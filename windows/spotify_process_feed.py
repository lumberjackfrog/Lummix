"""Windows' application-loopback capture, restricted to a verified Spotify process tree."""
from collections import deque
import ctypes
from ctypes import wintypes
import os
from pathlib import Path
import sys
import threading
import time

import numpy as np
import psutil

from cable_mixer import BLOCK
from spotify_cable_guard import direct_to_cable
from spotify_duck import spotify_path

DLL = (Path(__file__).resolve().parent if getattr(sys,"frozen",False) else Path(__file__).resolve().parents[1]/".build") / "lummix_process_audio.dll"
CALLBACK = ctypes.WINFUNCTYPE(None,ctypes.POINTER(ctypes.c_int16),ctypes.c_uint32,ctypes.c_uint32)


class AmbiguousTarget(Exception):
    pass


def supported():
    return os.name=="nt" and sys.getwindowsversion().build>=20348 and DLL.is_file()


def spotify_target():
    matches={}
    for proc in psutil.process_iter(["name"]):
        if (proc.info["name"] or "").casefold()!="spotify.exe":
            continue
        try:
            path=proc.exe()
            if spotify_path(path,os.environ):
                edition="store" if "windowsapps" in (p.lower() for p in Path(path).parts) else "desktop"
                matches[proc.pid]=(proc.ppid(),proc.create_time(),edition)
        except (OSError,psutil.Error):
            continue
    roots=[(pid,created,edition) for pid,(parent,created,edition) in matches.items() if parent not in matches]
    if len(roots)>1:
        raise AmbiguousTarget()
    return roots[0] if roots else None


def windows():
    k=ctypes.WinDLL("kernel32",use_last_error=True)
    k.OpenProcess.argtypes=[wintypes.DWORD,wintypes.BOOL,wintypes.DWORD]
    k.OpenProcess.restype=wintypes.HANDLE
    k.CreateEventW.argtypes=[ctypes.c_void_p,wintypes.BOOL,wintypes.BOOL,wintypes.LPCWSTR]
    k.CreateEventW.restype=wintypes.HANDLE
    k.SetEvent.argtypes=[wintypes.HANDLE]
    k.CloseHandle.argtypes=[wintypes.HANDLE]
    k.GetProcessTimes.argtypes=[wintypes.HANDLE]+[ctypes.POINTER(wintypes.FILETIME)]*4
    return k


class Capture:
    """One pinned process handle. Packet memory never leaves the mixer or writes to disk."""
    def __init__(self, target):
        pid,created,self.edition=target
        self.target=target
        self.blocks=deque(maxlen=6)
        self.tail=np.empty((0,2),np.float32)
        self.last_signal=0.
        self.attached=False
        self.accepting=True
        self.code=None
        self.k=windows()
        self.process=self.k.OpenProcess(0x00100000|0x1000,False,pid)
        if not self.process:
            raise OSError("Target unavailable")
        try:
            stamps=[wintypes.FILETIME() for _ in range(4)]
            if not self.k.GetProcessTimes(self.process,*(ctypes.byref(x) for x in stamps)):
                raise OSError("Target unavailable")
            stamp=stamps[0]
            actual=((stamp.dwHighDateTime<<32)|stamp.dwLowDateTime)/10_000_000-11644473600
            if abs(actual-created)>.001:
                raise OSError("Target restarted")
            self.dll=ctypes.WinDLL(str(DLL))
            self.dll.LumCaptureProcess.argtypes=[wintypes.HANDLE,wintypes.HANDLE,CALLBACK]
            self.dll.LumCaptureProcess.restype=ctypes.c_long
            self.event=self.k.CreateEventW(None,True,False,None)
            if not self.event:
                raise OSError("Capture unavailable")
        except BaseException:
            self.k.CloseHandle(self.process)
            raise
        self.callback=CALLBACK(self.packet)
        self.thread=threading.Thread(target=self.run,name="LumAppAudio",daemon=True)
        self.thread.start()

    def packet(self,data,frames,flags):
        if not self.accepting:return
        if not frames:
            self.attached=True
            return
        if flags&1:  # DATA_DISCONTINUITY: never bridge a gap with old partial samples.
            self.tail=np.empty((0,2),np.float32)
        if data:
            block=np.ctypeslib.as_array(data,shape=(frames*2,)).reshape((-1,2)).astype(np.float32)
            block *= 1/32768
        else:
            block=np.zeros((frames,2),np.float32)
        if self.tail.size:
            block=np.concatenate((self.tail,block),axis=0)
        complete=len(block)//BLOCK*BLOCK
        for idx in range(0,complete,BLOCK):
            unit=block[idx:idx+BLOCK]
            # Windows may dither digital silence by one 16-bit LSB.
            if np.max(unit) > 4/32768 or np.min(unit) < -4/32768:
                self.last_signal=time.monotonic()
                self.blocks.append(unit)
            else:
                self.blocks.append(None)
        self.tail=block[complete:]

    def run(self):
        self.code=self.dll.LumCaptureProcess(self.process,self.event,self.callback)

    def next(self):
        try:return self.blocks.popleft()
        except IndexError:return None

    def close(self):
        self.accepting=False
        self.k.SetEvent(self.event)
        self.thread.join()
        self.blocks.clear()
        self.k.CloseHandle(self.event)
        self.k.CloseHandle(self.process)


class SpotifyProcessFeed:
    """Only constructed for an explicitly started mix. Check routing before every attachment."""
    def __init__(self,signed_render_ids,*,discover=spotify_target,guard=direct_to_cable):
        self.ids=frozenset(signed_render_ids)
        self.discover,self.guard=discover,guard
        self.capture=None
        self.state="connecting" if supported() else "unsupported"
        self.edition=None
        self.stop=threading.Event()
        self.thread=None
        if self.state!="unsupported":
            self.thread=threading.Thread(target=self.watch,name="LumSpotifySource",daemon=True)
            self.thread.start()

    def next(self):
        cap=self.capture
        return cap.next() if cap and cap.accepting else None

    def detach(self):
        cap,self.capture=self.capture,None
        if cap:cap.close()

    def watch(self):
        try:
            while not self.stop.is_set():
                wait=.5
                try:
                    target=self.discover()
                    if self.capture and self.capture.target!=target:self.detach()
                    self.edition=target[2] if target else None
                    if not target:
                        self.state="waiting_spotify"
                    else:
                        blocked=self.guard(target[0],self.ids)
                        if blocked is not False:
                            self.detach()
                            self.state="direct_to_cable" if blocked is True else "unavailable"
                        else:
                            if self.capture is None and not self.stop.is_set():
                                self.state="connecting"
                                self.capture=Capture(target)
                            cap=self.capture
                            if cap and cap.code is not None:
                                self.detach()
                                self.state="unavailable"
                                wait=2
                            elif cap and cap.attached:
                                self.state="receiving" if time.monotonic()-cap.last_signal<1.2 else "waiting_audio"
                except AmbiguousTarget:
                    self.detach()
                    self.edition=None
                    self.state="ambiguous"
                except Exception:
                    self.detach()
                    self.state="unavailable"
                    wait=2
                self.stop.wait(wait)
        finally:
            self.detach()
            self.state="off"
            self.edition=None

    def close(self):
        self.stop.set()
        if self.thread:self.thread.join()
