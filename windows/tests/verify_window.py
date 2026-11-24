"""Exercise the shipping double-click entry on a separate, never-input Win32 desktop."""
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import uuid

from PIL import Image
from playwright.sync_api import sync_playwright
import psutil
import win32api, win32con, win32event, win32gui, win32process, win32security, win32ui

HERE = Path(__file__).resolve().parent
EVIDENCE = HERE.parents[1] / ".build/qa"
EVIDENCE.mkdir(parents=True, exist_ok=True)
exe = Path(sys.argv[1]).resolve() if len(sys.argv)>1 else HERE.parents[1]/".build/frozen/runtime/Lummix Helper.exe"
source_run = "--source" in sys.argv[2:]
window_only = "--window-only" in sys.argv[2:]
u = ctypes.WinDLL("user32", use_last_error=True)
u.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))
u.CreateDesktopW.argtypes = [wintypes.LPCWSTR,wintypes.LPCWSTR,ctypes.c_void_p,wintypes.DWORD,wintypes.DWORD,ctypes.c_void_p]
u.CreateDesktopW.restype = wintypes.HANDLE
u.CloseDesktop.argtypes = [wintypes.HANDLE]
u.ShowWindowAsync.argtypes = [wintypes.HWND, ctypes.c_int]
def until(predicate, limit=15):
    end=time.monotonic()+limit
    while time.monotonic()<end:
        if predicate(): return
        time.sleep(.12)
    raise AssertionError("Timed out waiting for "+str(predicate))

name="LummixNativeQA_"+uuid.uuid4().hex
desk=u.CreateDesktopW(name,None,None,0,0x000F01FF,None)
if not desk: raise ctypes.WinError(ctypes.get_last_error())
windows=[]
def handles():
    windows.clear()
    def add(hwnd,_):
        if win32gui.GetClassName(hwnd)=="#32770" and win32gui.GetWindowText(hwnd)=="Lummix":
            texts=[]
            win32gui.EnumChildWindows(hwnd,lambda h,_:texts.append(win32gui.GetWindowText(h)) or True,None)
            raise RuntimeError("Lummix dialog: "+" ".join(texts))
        if win32gui.IsWindowVisible(hwnd) and win32gui.GetWindowText(hwnd).startswith("Lummix") and win32gui.GetClassName(hwnd)=="LumMixStudioHost": windows.append(hwnd)
        return True
    win32gui.EnumDesktopWindows(desk,add,None)
    return list(windows)
def services():
    return {str(x.laddr.port):x.pid for x in psutil.net_connections("tcp") if x.status=="LISTEN" and x.laddr.ip=="127.0.0.1" and x.laddr.port in (41835,41836,41838)}

def check_frame(hwnd):
    left,top,right,bottom=win32gui.GetWindowRect(hwnd)
    assert win32gui.ClientToScreen(hwnd,(0,0))==(left,top)
    assert win32gui.GetClientRect(hwnd)==(0,0,right-left,bottom-top)
    edge=round(5*u.GetDpiForWindow(hwnd)/96)
    midx,midy=(left+right)//2,(top+bottom)//2
    fixtures=((left,top,13),(midx,top,12),(right-1,top,14),
              (left,midy,10),(right-1,midy,11),
              (left,bottom-1,16),(midx,bottom-1,15),(right-1,bottom-1,17),
              (left+edge-1,midy,10),(left+edge,midy,1))
    for x,y,code in fixtures:
        point=((y&0xffff)<<16)|(x&0xffff)
        assert win32gui.SendMessage(hwnd,win32con.WM_NCHITTEST,0,point)==code,(x,y,code)

def client_bounds(hwnd):
    l,t,r,b=win32gui.GetClientRect(hwnd)
    return (*win32gui.ClientToScreen(hwnd,(l,t)),*win32gui.ClientToScreen(hwnd,(r,b)))
def screenshot(hwnd):
    left,top,right,bottom=win32gui.GetWindowRect(hwnd);w,h=right-left,bottom-top
    dc=win32gui.GetWindowDC(hwnd);src=win32ui.CreateDCFromHandle(dc);mem=src.CreateCompatibleDC()
    bitmap=win32ui.CreateBitmap();bitmap.CreateCompatibleBitmap(src,w,h);mem.SelectObject(bitmap)
    try:
        if not u.PrintWindow(hwnd,mem.GetSafeHdc(),2): raise AssertionError("PrintWindow failed")
        Image.frombuffer("RGB",(w,h),bitmap.GetBitmapBits(True),"raw","BGRX",0,1).save(EVIDENCE/"native-window.png")
    finally:
        mem.DeleteDC();src.DeleteDC();win32gui.ReleaseDC(hwnd,dc);win32gui.DeleteObject(bitmap.GetHandle())
before=services();proc=thread=None;hwnd=None
try:
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as tmp:
        env=dict(os.environ,LOCALAPPDATA=tmp)
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1",0));debug_port=reserved.getsockname()[1]
        env["WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"]="--remote-debugging-port="+str(debug_port)
        cfg=Path(tmp)/"LumMix/studio-preferences.json";cfg.parent.mkdir()
        env["PATH"]=os.environ["SystemRoot"]+"\\System32;"+os.environ["SystemRoot"]
        si=win32process.STARTUPINFO();si.lpDesktop="winsta0\\"+name
        def launch():
            args = ["-c","import lummix_window; lummix_window.launch()"] if source_run else (["launch"] if "Helper" in exe.name else [])
            if window_only:args=["-c","import ctypes;ctypes.windll.user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4));from studio_window import run;run()"]
            cwd = HERE.parent/"runtime" if source_run or window_only else exe.parent
            cmdfile = exe.suffix.lower()==".cmd"
            app = os.environ["ComSpec"] if cmdfile else str(exe)
            command = subprocess.list2cmdline([app])+' /d /c ""'+str(exe)+'""' if cmdfile else subprocess.list2cmdline([str(exe),*args])
            return win32process.CreateProcess(app,command,None,None,False,
                win32con.CREATE_UNICODE_ENVIRONMENT|win32con.CREATE_NO_WINDOW,env,str(cwd),si)
        proc,thread,pid,_=launch()
        until(handles,38);hwnd=windows[0]
        time.sleep(2)
        host = next((p for p in psutil.process_iter(["name","exe"]) if p.info["name"]=="Lummix Helper.exe"
            and p.info["exe"] and Path(p.info["exe"])==exe.parent/"runtime/Lummix Helper.exe" and "launch" in p.cmdline()),None)
        board_title=win32gui.GetWindowText(hwnd)
        screenshot(hwnd)
        children=[];win32gui.EnumChildWindows(hwnd,lambda h,_:children.append(win32gui.GetClassName(h)) or True,None)
        assert any("Chrome" in cls for cls in children),children
        style=win32gui.GetWindowLong(hwnd,win32con.GWL_STYLE)
        assert not style & win32con.WS_CAPTION, "A native caption would duplicate the app header."
        for flag in (win32con.WS_MINIMIZEBOX,win32con.WS_MAXIMIZEBOX,win32con.WS_THICKFRAME,win32con.WS_SYSMENU):
            assert style&flag
        u.ShowWindowAsync(hwnd,win32con.SW_MINIMIZE);until(lambda:win32gui.IsIconic(hwnd))
        u.ShowWindowAsync(hwnd,win32con.SW_RESTORE);until(lambda:not win32gui.IsIconic(hwnd))
        u.ShowWindowAsync(hwnd,win32con.SW_MAXIMIZE);until(lambda:u.IsZoomed(hwnd))
        u.ShowWindowAsync(hwnd,win32con.SW_RESTORE);until(lambda:not u.IsZoomed(hwnd))
        dpi=[]
        for mon,*_ in win32api.EnumDisplayMonitors():
            left,top,right,bottom=win32api.GetMonitorInfo(mon)["Work"]
            win32gui.SetWindowPos(hwnd,None,left+40,top+40,850,620,win32con.SWP_NOZORDER|win32con.SWP_NOACTIVATE)
            time.sleep(.2);dpi.append(u.GetDpiForWindow(hwnd))
            check_frame(hwnd)
            win32gui.PostMessage(hwnd,win32con.WM_SYSCOMMAND,win32con.SC_MAXIMIZE,0)
            until(lambda:client_bounds(hwnd)==(left,top,right,bottom))
            win32gui.PostMessage(hwnd,win32con.WM_SYSCOMMAND,win32con.SC_RESTORE,0)
            until(lambda:not u.IsZoomed(hwnd))
        win32gui.SetWindowPos(hwnd,None,24,24,800,600,win32con.SWP_NOZORDER|win32con.SWP_NOACTIVATE)
        time.sleep(.25)
        win32gui.SetWindowPos(hwnd,None,24,24,800,600,win32con.SWP_NOZORDER|win32con.SWP_NOACTIVATE)
        l,t,r,b=win32gui.GetWindowRect(hwnd); assert (r-l,b-t)==(800,600)
        prior=win32gui.GetWindowRect(hwnd)
        # Verify commands from the actual local renderer and that explicit Exit
        # awaits the page's ownership-release hook before tearing WebView down.
        def page_for(browser):
            return next((page for context in browser.contexts for page in context.pages
                         if page.url.startswith("http://127.0.0.1:41835/")),None)
        with sync_playwright() as pw:
            session=pw.chromium.connect_over_cdp("http://127.0.0.1:"+str(debug_port))
            until(lambda:page_for(session))
            page=page_for(session)
            board=page.frame_locator("#canvas")
            maximize=board.locator("#window-maximize")
            maximize.click(timeout=12000)
            work=win32api.GetMonitorInfo(win32api.MonitorFromWindow(hwnd,2))["Work"]
            until(lambda:u.IsZoomed(hwnd) and client_bounds(hwnd)==work)
            assert "Restore" in maximize.get_attribute("aria-label")
            board.locator("#window-fullscreen").click()
            monitor=win32api.GetMonitorInfo(win32api.MonitorFromWindow(hwnd,2))["Monitor"]
            until(lambda:win32gui.GetWindowRect(hwnd)==monitor)
            board.locator("#window-fullscreen").click()
            until(lambda:u.IsZoomed(hwnd) and client_bounds(hwnd)==work)
            maximize.click()
            until(lambda:win32gui.GetWindowRect(hwnd)==prior)
            board.locator("#window-drag").dblclick()
            until(lambda:u.IsZoomed(hwnd))
            maximize.click()
            until(lambda:win32gui.GetWindowRect(hwnd)==prior)
            board.locator("#window-minimize").click()
            until(lambda:win32gui.IsIconic(hwnd))
            u.ShowWindowAsync(hwnd,win32con.SW_RESTORE)
            until(lambda:win32gui.GetWindowRect(hwnd)==prior)
            board.locator("#window-fullscreen").click()
            until(lambda:not win32gui.GetWindowLong(hwnd,win32con.GWL_STYLE)&win32con.WS_THICKFRAME)
            board.locator("#window-fullscreen").click()
            until(lambda:win32gui.GetWindowRect(hwnd)==prior)
        win32gui.PostMessage(hwnd,win32con.WM_KEYDOWN,win32con.VK_F11,0)
        until(lambda:not win32gui.GetWindowLong(hwnd,win32con.GWL_STYLE)&win32con.WS_THICKFRAME)
        monitor=win32api.GetMonitorInfo(win32api.MonitorFromWindow(hwnd,2))["Monitor"]
        assert win32gui.GetWindowRect(hwnd)==monitor
        win32gui.PostMessage(hwnd,win32con.WM_KEYDOWN,win32con.VK_ESCAPE,0)
        until(lambda:win32gui.GetWindowRect(hwnd)==prior)
        hp=win32api.OpenProcess(0x1000,False,win32process.GetWindowThreadProcessId(hwnd)[1])
        token=win32security.OpenProcessToken(hp,win32con.TOKEN_QUERY)
        try: elevated=bool(win32security.GetTokenInformation(token,win32security.TokenElevation))
        finally: token.Close();hp.Close()
        assert not elevated
        live=services()
        # Default close uses a real shell icon if this desktop has a notification area.
        win32gui.PostMessage(hwnd,win32con.WM_CLOSE,0,0)
        until(lambda:not win32gui.IsWindowVisible(hwnd))
        tray=bool(win32gui.IsWindow(hwnd))
        if tray:
            win32gui.PostMessage(hwnd,win32con.WM_APP+290,0,win32con.WM_LBUTTONUP)
            until(lambda:win32gui.IsWindowVisible(hwnd))
            win32gui.PostMessage(hwnd,win32con.WM_CLOSE,0,0)
            until(lambda:not win32gui.IsWindowVisible(hwnd))
            if window_only:
                win32gui.PostMessage(hwnd,win32con.WM_COMMAND,0x7120,0)
            else:
                p2,t2,_,_=launch();assert win32event.WaitForSingleObject(p2,5000)==0
                p2.Close();t2.Close()
            until(lambda:handles()==[hwnd])
            win32gui.PostMessage(hwnd,win32con.WM_COMMAND,0x7140,0)
        assert win32event.WaitForSingleObject(proc,6000)==0
        assert win32process.GetExitCodeProcess(proc)==0
        if host: host.wait(timeout=8)
        until(lambda:services()==before)
        proc.Close();thread.Close();proc=thread=None;hwnd=None
        cfg.write_text('{"settings":{"closeToTray":false}}')
        proc,thread,pid,_=launch();until(handles,35);hwnd=windows[0]
        until(lambda:win32gui.GetWindowRect(hwnd)==prior)
        if not window_only:
            p2,t2,_,_=launch();assert win32event.WaitForSingleObject(p2,5000)==0 and handles()==[hwnd]
            p2.Close();t2.Close()
        with sync_playwright() as pw:
            session=pw.chromium.connect_over_cdp("http://127.0.0.1:"+str(debug_port))
            until(lambda:page_for(session))
            page=page_for(session)
            page.wait_for_function("typeof document.querySelector('#canvas')?.contentWindow?.releaseLummixAudio === 'function'")
            page.evaluate("""() => {
                const child=document.querySelector('#canvas').contentWindow;
                const release=child.releaseLummixAudio;
                child.releaseLummixAudio=async () => {
                    await release();
                    await new Promise(resolve=>setTimeout(resolve,550));
                    localStorage.setItem('lummix-test-release-complete','yes');
                };
            }""")
            released_since=time.monotonic()
            page.frame_locator("#canvas").locator("#window-close").click(no_wait_after=True)
        assert win32event.WaitForSingleObject(proc,7000)==0
        release_duration=time.monotonic()-released_since
        assert .5<=release_duration<3.5,release_duration
        result=dict(privateDesktop=True,userDesktopUntouched=True,launcher=hashlib.sha256(exe.read_bytes()).hexdigest(),
            studio=board_title,renderer="Microsoft Edge WebView2",view="Desktop",controls=["minimize","restore","maximize","restore","resize 800x600","F11 fullscreen","Escape restore","close"],
            monitorDpi=dpi,webFullscreenBridge=True,eightResizeRegions=True,headerClickActions=True,
            doubleClickMaximizes=True,monitorWorkAreas=True,awaitedExitHookSeconds=round(release_duration,2),
            defaultTrayAvailableOnPrivateDesktop=tray,defaultCloseWithNoShellExits=not tray,
            restoredFromTrayOnSameHandle=tray,explicitExitFromTray=tray,settingOffCloseExits=True,restoredPlacement=prior,
            singleWindow=not window_only,elevated=elevated,reusedOrStarted=live,preexistingServicePidsPreserved=before,pythonRemovedFromPath=True)
        print(json.dumps(result))
        (EVIDENCE/"native-window.json").write_text(json.dumps(result,indent=2),encoding="utf-8")
finally:
    if hwnd and win32gui.IsWindow(hwnd): win32gui.PostMessage(hwnd,win32con.WM_COMMAND,0x7140,0)
    if thread:thread.Close()
    if proc:
        if win32event.WaitForSingleObject(proc,1500)!=0:win32process.TerminateProcess(proc,2)
        proc.Close()
    u.CloseDesktop(desk)
