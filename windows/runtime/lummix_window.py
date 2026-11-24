"""Start or reuse identified local services and open the native studio window."""
import ctypes
from ctypes import wintypes as wt
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import time


ORIGIN = "http://127.0.0.1:41835"
SERVICES = {"studio":41835, "wrist":41836, "spotify":41838}
MEDIA_CAPABILITIES = {"spotify-session-peak-v1", "mic-route-v1", "mixer-duck-v1", "studio-preferences-v1",
                      "mix-setup-alias-v1"}
if sys.getwindowsversion().build >= 20348:
    MEDIA_CAPABILITIES.add("spotify-process-mix-v1")
MEDIA_CONFLICT = ("Port 41838 is already in use by an incompatible service. Close any earlier Lummix "
                 "window or stop its media helper, then reopen this version of Lummix.")
user = ctypes.WinDLL("user32", use_last_error=True)
kernel = ctypes.WinDLL("kernel32", use_last_error=True)
kernel.CreateMutexW.argtypes = [ctypes.c_void_p, wt.BOOL, wt.LPCWSTR]
kernel.CreateMutexW.restype = wt.HANDLE
kernel.CloseHandle.argtypes = [wt.HANDLE]
kernel.WaitForSingleObject.argtypes = [wt.HANDLE, wt.DWORD]
kernel.ReleaseMutex.argtypes = [wt.HANDLE]

def request(port,path,method="GET",headers=None,limit=131072):
    conn = HTTPConnection("127.0.0.1",port,timeout=1.5)
    try:
        conn.request(method,path,headers=headers or {})
        reply=conn.getresponse()
        return reply.status,reply.headers,reply.read(limit)
    finally: conn.close()

def matches(name,port):
    try:
        if name=="studio":
            return b"design-board.js" in request(port,"/")[2] and b'id="lum-visualizer"' in request(port,"/Mixer%20preview.html")[2]
        if name=="wrist":
            code,_,body=request(port,"/lummix-health")
            if code==200:
                health=json.loads(body)
                return health.get("service")=="lummix-wrist" and health.get("ready") is True
            code,headers,_=request(port,"/preview")
            return (code==302 and headers.get("Location")=="/vendor/_UI/Default/Wrist.html" and
                    re.fullmatch(rb"\d+-\d+",request(port,"/revision")[2]) is not None)
        code,headers,body=request(port,"/lummix-health",headers={"Origin":ORIGIN},limit=4096)
        if code!=200 or headers.get("Access-Control-Allow-Origin")!=ORIGIN: return False
        health=json.loads(body)
        return (isinstance(health,dict) and health.get("service")=="lummix-media"
                and health.get("protocol")==2 and isinstance(health.get("capabilities"),list)
                and all(cap in health["capabilities"] for cap in MEDIA_CAPABILITIES))
    except (OSError,ValueError,AttributeError): return False

def listening(port):
    try:
        with socket.create_connection(("127.0.0.1",port),timeout=.4): return True
    except OSError: return False

def ensure(name,port,helper,owned):
    if listening(port):
        if matches(name,port): return
        if name=="spotify": raise RuntimeError(MEDIA_CONFLICT)
        raise RuntimeError(f"Lummix could not identify the service on port {port}. Close the conflicting app or try again after startup.")
    if not helper.is_file():
        raise RuntimeError("Extract the complete Lummix ZIP into one folder and open Lummix again.")
    child=subprocess.Popen([str(helper),name,"--parent-stdin"],stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,creationflags=subprocess.CREATE_NO_WINDOW)
    owned.append(child)
    deadline=time.monotonic()+22
    while time.monotonic()<deadline:
        if matches(name,port): return
        if child.poll() is not None: break
        time.sleep(.18)
    raise RuntimeError(f"The {name} service did not start on port {port}. Extract the complete download and try again.")

def launch():
    if getattr(sys,"frozen",False):
        # Only the local service modes need a console pipe.
        kernel.FreeConsole()
    user.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))
    import win32gui
    from studio_window import CAN_REOPEN, CLASS, OPEN, run
    mutex=kernel.CreateMutexW(None,True,r"Local\LummixNativeStudio")
    if not mutex: raise ctypes.WinError(ctypes.get_last_error())
    already=ctypes.get_last_error()==183
    owns_mutex=not already
    owned=[]
    try:
        if already:
            for _ in range(60):
                found=win32gui.FindWindow(CLASS,None)
                if found and win32gui.SendMessageTimeout(found,CAN_REOPEN,0,0,2,300)[1]:
                    if not matches("spotify",SERVICES["spotify"]):
                        raise RuntimeError(MEDIA_CONFLICT)
                    from win32process import GetWindowThreadProcessId
                    user.AllowSetForegroundWindow(GetWindowThreadProcessId(found)[1])
                    win32gui.PostMessage(found,0x0111,OPEN,0);return
                # Closing and immediately reopening should wait for owned audio routes to release.
                if kernel.WaitForSingleObject(mutex,180) in (0,0x80):
                    owns_mutex=True
                    break
            if not owns_mutex: return
        helper=Path(sys.executable).resolve()
        if not getattr(sys,"frozen",False):
            helper=Path(__file__).resolve().parents[2]/".build/frozen/runtime/Lummix Helper.exe"
        for name,port in SERVICES.items(): ensure(name,port,helper,owned)
        from runtime_main import DATA, wrist_sources
        from xsoverlay_service import serve_with_window
        with serve_with_window(wrist_sources, DATA):
            run()
    except Exception as exc:
        user.MessageBoxW(None,str(exc),"Lummix",0x10)
        raise SystemExit(1) from exc
    finally:
        for child in owned:
            try:child.stdin.close()
            except OSError:pass
        until=time.monotonic()+6
        for child in owned:
            try:child.wait(timeout=max(.02,until-time.monotonic()))
            except subprocess.TimeoutExpired:pass
        if owns_mutex:kernel.ReleaseMutex(mutex)
        kernel.CloseHandle(mutex)
