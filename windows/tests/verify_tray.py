"""Exercise our disposable window's real Explorer notification icon without taking focus."""
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

import win32api
import win32con as c
import win32gui as gui
import win32process

HERE = Path(__file__).resolve().parent
EVIDENCE = HERE.parents[1] / ".build/qa"
EVIDENCE.mkdir(parents=True, exist_ok=True)
RUNTIME = HERE.parent / "portable-runtime"
CLASS = "LumMixStudioHost"
class Identifier(ctypes.Structure):
    _fields_ = [("size",wintypes.DWORD),("hwnd",wintypes.HWND),("uid",wintypes.UINT),("guid",ctypes.c_ubyte*16)]

def icon_rect(hwnd):
    info=Identifier(ctypes.sizeof(Identifier),hwnd,1)
    rect=wintypes.RECT()
    result=ctypes.windll.shell32.Shell_NotifyIconGetRect(ctypes.byref(info),ctypes.byref(rect))
    return result, (rect.left,rect.top,rect.right,rect.bottom)

def until(fn, timeout=15):
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        value=fn()
        if value:return value
        time.sleep(.08)
    raise AssertionError("Tray condition timed out at "+str(fn.__code__.co_firstlineno))

def hosts(pid=None):
    found=[]
    def check(hwnd,_):
        if gui.GetClassName(hwnd)==CLASS and (pid is None or win32process.GetWindowThreadProcessId(hwnd)[1]==pid):
            found.append(hwnd)
        return True
    gui.EnumWindows(check,None)
    return found

def main():
    ctypes.windll.user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))
    assert not hosts(),"An existing Lummix native window is already on this desktop"
    assert gui.FindWindow("Shell_TrayWnd",None),"Explorer is unavailable"
    initial_focus=gui.GetForegroundWindow()
    native=Path(sys.argv[1]).resolve() if len(sys.argv)>1 else None
    args=[str(native)] if native else [sys.executable,"-c","from studio_window import run;run()"]
    if native and native.name=="Lummix Helper.exe":args.append("launch")
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as tmp:
        env={**os.environ,"LOCALAPPDATA":tmp}
        si=subprocess.STARTUPINFO();si.dwFlags=subprocess.STARTF_USESHOWWINDOW;si.wShowWindow=c.SW_HIDE
        def start():
            return subprocess.Popen(args,cwd=RUNTIME if not native else native.parent,env=env,startupinfo=si,
                                    creationflags=subprocess.CREATE_NO_WINDOW)
        p=start();hwnd=None
        try:
            hwnd=until(lambda:next(iter(hosts(p.pid)),None))
            assert not gui.IsWindowVisible(hwnd),"QA window did not honor hidden Win32 startup"
            gui.EnableWindow(hwnd,False)
            style=gui.GetWindowLong(hwnd,c.GWL_EXSTYLE)
            gui.SetWindowLong(hwnd,c.GWL_EXSTYLE,style|c.WS_EX_NOACTIVATE|c.WS_EX_TOOLWINDOW)
            x=win32api.GetSystemMetrics(76)+win32api.GetSystemMetrics(78)+4000
            gui.SetWindowPos(hwnd,None,x,0,600,450,c.SWP_NOACTIVATE|c.SWP_NOZORDER)
            time.sleep(.18) # finish any inter-monitor DPI transition while invisible
            gui.SetWindowPos(hwnd,None,x,0,600,450,c.SWP_NOACTIVATE|c.SWP_NOZORDER)
            gui.PostMessage(hwnd,c.WM_CLOSE,0,0)
            until(lambda:icon_rect(hwnd)[0]==0)
            assert p.poll() is None and not gui.IsWindowVisible(hwnd)
            rect=icon_rect(hwnd)[1]
            gui.PostMessage(hwnd,c.WM_APP+290,0,c.WM_USER)
            until(lambda:gui.IsWindowVisible(hwnd))
            assert not ctypes.windll.user32.MonitorFromWindow(hwnd,0),gui.GetWindowRect(hwnd)
            gui.PostMessage(hwnd,c.WM_CLOSE,0,0)
            until(lambda:not gui.IsWindowVisible(hwnd))
            if native:
                relaunch=start()
                assert relaunch.wait(12)==0
                until(lambda:gui.IsWindowVisible(hwnd))
                assert hosts()==[hwnd]
                gui.PostMessage(hwnd,c.WM_CLOSE,0,0)
                until(lambda:not gui.IsWindowVisible(hwnd))
            gui.PostMessage(hwnd,c.WM_COMMAND,0x7140,0)
            assert p.wait(12)==0 and not gui.IsWindow(hwnd) and icon_rect(hwnd)[0]!=0
            report={"registeredWithExplorer":True,"rect":rect,"trayOpenReusedHwnd":True,"relaunchReusedHiddenHwnd":True if native else None,
                    "exitCode":p.returncode,"iconRemovedOnExit":True,"foregroundPreserved":gui.GetForegroundWindow()==initial_focus}
            assert report["foregroundPreserved"],report
            (EVIDENCE/"native-tray.json").write_text(json.dumps(report,indent=2))
            print(json.dumps(report))
        finally:
            if hwnd and gui.IsWindow(hwnd):gui.PostMessage(hwnd,c.WM_COMMAND,0x7140,0)
            if p.poll() is None:
                try:p.wait(8)
                except subprocess.TimeoutExpired:p.terminate()

if __name__=="__main__":main()
