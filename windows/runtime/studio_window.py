"""Windows-owned studio window, notification icon and Microsoft WebView2 surface."""
import ctypes
from ctypes import wintypes as wt
from functools import cache
import json
import os
from pathlib import Path
import secrets
import sys
from urllib.parse import urlsplit
from native_host import library
from native_webview import WebView, READY as WEBVIEW_READY, ERROR as WEBVIEW_ERROR, MESSAGE, NEW_PAGE, KEY
import profile_save
import diagnostics
from client_update import Updates
from spotify_setup import SpotifySetup

import win32api
import win32con as wc
import win32gui as gui

CLASS = "LumMixStudioHost"
OPEN, FULL, EXIT, MINIMIZE, MAXIMIZE, DRAG, RESIZE = 0x7120, 0x7130, 0x7140, 0x7150, 0x7160, 0x7170, 0x7180
WINDOW_COMMANDS = {"fullscreen": FULL, "exit": EXIT, "minimize": MINIMIZE, "maximize": MAXIMIZE, "drag": DRAG}
RESIZE_COMMANDS = {"resize-nw": wc.HTTOPLEFT, "resize-n": wc.HTTOP, "resize-ne": wc.HTTOPRIGHT,
                   "resize-w": wc.HTLEFT, "resize-e": wc.HTRIGHT,
                   "resize-sw": wc.HTBOTTOMLEFT, "resize-s": wc.HTBOTTOM, "resize-se": wc.HTBOTTOMRIGHT}
TRAY = wc.WM_APP + 290
READY = wc.WM_APP + 291
FINISH_EXIT = wc.WM_APP + 292
CAN_REOPEN = wc.WM_APP + 293
UPDATE_CHANGED = wc.WM_APP + 294
SPOTIFY_SETUP_CHANGED = wc.WM_APP + 295
EXIT_TIMER = 17
NATIVE = "http://127.0.0.1:41835/?view=desktop&app=1"
USER = ctypes.windll.user32
PROJECT = Path(__file__).resolve().parent.parent

@cache
def _native_shell():
    host = library()
    host.lummix_tray_show.argtypes = [wt.HWND, wt.HICON, wt.UINT, wt.BOOL]
    host.lummix_tray_show.restype = wt.BOOL
    host.lummix_tray_hide.argtypes = [wt.HWND, wt.HICON, wt.UINT]
    host.lummix_tray_hide.restype = None
    host.lummix_tray_menu.argtypes = [wt.HWND, ctypes.c_size_t, ctypes.c_size_t, ctypes.c_size_t,
                                     ctypes.c_int32, ctypes.c_int32]
    host.lummix_tray_menu.restype = None
    host.lummix_frame_edge.argtypes = [wt.HWND, wt.BOOL]
    host.lummix_frame_edge.restype = ctypes.c_int32
    host.lummix_frame_message.argtypes = [wt.HWND, wt.UINT, wt.LPARAM, wt.BOOL]
    host.lummix_frame_message.restype = ctypes.c_ssize_t
    return host

def local_dir():
    return Path(os.environ["LOCALAPPDATA"])/"LumMix"

def window_settings(data):
    for key in ("x", "y", "width", "height"):
        if type(data.get(key)) not in (int, float) or not abs(data[key]) < 20000:
            raise ValueError("Invalid saved window dimensions")

def read_json(path):
    try:
        if path.name == "studio-preferences.json":
            from studio_preferences import checked
            return profile_save.load(path, lambda data: checked(data["settings"]))
        return profile_save.load(path, window_settings)
    except (OSError, ValueError, KeyError, TypeError):
        return {}

def close_to_tray():
    settings=read_json(local_dir()/"studio-preferences.json").get("settings")
    return not isinstance(settings,dict) or settings.get("closeToTray") is not False

def monitor_dpi(monitor):
    x,y=wt.UINT(96),wt.UINT(96)
    try:
        ctypes.windll.shcore.GetDpiForMonitor(wt.HANDLE(int(monitor)),0,ctypes.byref(x),ctypes.byref(y))
    except (AttributeError,OSError):pass
    return x.value or 96

def placement():
    data=read_json(local_dir()/"window-placement.json")
    monitors=[item[0] for item in win32api.EnumDisplayMonitors()]
    try:point=gui.GetCursorPos()
    except Exception:point=(0,0)
    monitor=next((m for m in monitors if win32api.GetMonitorInfo(m).get("Device")==data.get("screen")),
                 win32api.MonitorFromPoint(point,2))
    l,t,r,b=win32api.GetMonitorInfo(monitor)["Work"]
    factor=monitor_dpi(monitor)/96
    def number(key,default):
        val=data.get(key)
        return round(val*factor) if type(val) in (int,float) and abs(val)<20000 else default
    width=min(r-l,max(330,number("width",round(1280*factor))))
    height=min(b-t,max(360,number("height",round(830*factor))))
    x=min(r-width, max(l,l+number("x",(r-l-width)//2)))
    y=min(b-height,max(t,t+number("y",(b-t-height)//2)))
    return (x,y,width,height),data.get("maximized") is True

def data_file(name):
    if getattr(sys,"frozen",False):return Path(sys._MEIPASS)/name
    if name=="WebView2Loader.dll":return PROJECT.parent/".build/webview2"/name
    return PROJECT/"packaging"/name

class StudioWindow:
    def __init__(self):
        self.native = _native_shell()
        self.hwnd=0;self.tray=False;self.full=False;self.exiting=False
        self.exit_nonce=None
        self.web=None
        self.last_window_state=None
        self.native_error=None
        self.shell_restart=gui.RegisterWindowMessage("TaskbarCreated")
        self.icon=gui.LoadImage(0,str(data_file("Lummix.ico")),wc.IMAGE_ICON,32,32,wc.LR_LOADFROMFILE)
        self.small_icon=gui.LoadImage(0,str(data_file("Lummix.ico")),wc.IMAGE_ICON,16,16,wc.LR_LOADFROMFILE)
        wcinfo=gui.WNDCLASS();wcinfo.hInstance=win32api.GetModuleHandle(None);wcinfo.lpszClassName=CLASS
        wcinfo.lpfnWndProc=self.message;wcinfo.hIcon=self.icon
        wcinfo.style=wc.CS_DBLCLKS
        wcinfo.hCursor=gui.LoadCursor(0,wc.IDC_ARROW)
        self.frame_brush=gui.CreateSolidBrush(win32api.RGB(24,22,26))
        wcinfo.hbrBackground=self.frame_brush
        gui.RegisterClass(wcinfo)
        pos,maxed=placement()
        style=wc.WS_POPUP | (wc.WS_OVERLAPPEDWINDOW & ~wc.WS_CAPTION) | wc.WS_CLIPCHILDREN
        self.hwnd=gui.CreateWindowEx(0,CLASS,"Lummix",style,
            *pos,0,0,wcinfo.hInstance,None)
        self.updates=Updates(local_dir(),self.update_changed)
        self.spotify_setup=SpotifySetup(local_dir(),self.spotify_setup_changed)
        if sys.getwindowsversion().build >= 22000:
            # Ask the compositor to omit the accent outline on active and inactive windows.
            no_border = wt.DWORD(0xfffffffe)  # DWMWA_COLOR_NONE
            ctypes.windll.dwmapi.DwmSetWindowAttribute(
                wt.HWND(self.hwnd), 34, ctypes.byref(no_border), ctypes.sizeof(no_border))  # DWMWA_BORDER_COLOR
        gui.SendMessage(self.hwnd,wc.WM_SETICON,0,self.small_icon)
        self.normal=gui.GetWindowRect(self.hwnd)
        menu=gui.GetSystemMenu(self.hwnd,False)
        gui.AppendMenu(menu,wc.MF_SEPARATOR,0,"")
        gui.AppendMenu(menu,wc.MF_STRING,FULL,"Full screen\tF11")
        gui.AppendMenu(menu,wc.MF_STRING,EXIT,"Exit Lummix")
        gui.ShowWindow(self.hwnd,wc.SW_SHOWMAXIMIZED if maxed else wc.SW_SHOWNORMAL)
        gui.UpdateWindow(self.hwnd)

    def start(self):
        # WebView2 uses an app-owned profile. No personal browser profile is accessed.
        studio_root = Path(sys.executable).parent if getattr(sys, "frozen", False) else PROJECT.parent
        self.web = WebView(self.hwnd, data_file("WebView2Loader.dll"), local_dir() / "Studio WebView",
                           studio_root / "studio", self.web_event)

    def web_event(self, kind, source, data, detail):
        if kind == WEBVIEW_READY:
            self.updates.command("auto")
        elif kind == WEBVIEW_ERROR:
            self.native_error = data
            gui.PostMessage(self.hwnd, READY, 1, 0)
        elif kind == MESSAGE:
            self.web_message(source, data)
        elif kind == NEW_PAGE:
            self.new_page(data)
        elif kind == KEY and ((detail & 0xffff) == wc.VK_F11 or self.full):
            if not detail >> 16:
                gui.PostMessage(self.hwnd, wc.WM_COMMAND, FULL, 0)
            return True
        return False

    def update_changed(self):
        if self.hwnd and not self.exiting:
            try: gui.PostMessage(self.hwnd,UPDATE_CHANGED,0,0)
            except OSError: pass

    def spotify_setup_changed(self):
        if self.hwnd and not self.exiting:
            try: gui.PostMessage(self.hwnd,SPOTIFY_SETUP_CHANGED,0,0)
            except OSError: pass

    def new_page(self, uri):
        target = urlsplit(uri)
        if uri=="spotify:" or (target.scheme=="https" and
            target.hostname in {"vb-audio.com","www.vb-audio.com","www.spotify.com","open.spotify.com"}):
            try:os.startfile(uri)
            except OSError:pass

    def web_message(self, source, command):
        if self.exit_nonce and command=="released:"+self.exit_nonce:
            gui.PostMessage(self.hwnd,FINISH_EXIT,0,0)
        elif command in WINDOW_COMMANDS:gui.PostMessage(self.hwnd,wc.WM_COMMAND,WINDOW_COMMANDS[command],0)
        elif command in RESIZE_COMMANDS:gui.PostMessage(self.hwnd,wc.WM_COMMAND,RESIZE,RESIZE_COMMANDS[command])
        elif command=="close":gui.PostMessage(self.hwnd,wc.WM_CLOSE,0,0)
        elif command=="window-state":self.send_window_state(force=True)
        elif command in ("spotify-setup","spotify-setup-state") and source==NATIVE and not self.exiting:
            if command=="spotify-setup":
                self.spotify_setup.begin()
            self.web.post_json(json.dumps(self.spotify_setup.status()))
        elif command in ("update-state","update-check","update-download","update-install") and source==NATIVE and not self.exiting:
            if command!="update-state":
                self.updates.command(command.removeprefix("update-"))
            self.web.post_json(json.dumps(self.updates.status()))
        elif command in ("logs-open","logs-clear") and source==NATIVE:
            try:
                if command=="logs-open":
                    folder=diagnostics.folder()
                    folder.mkdir(exist_ok=True,parents=True)
                    diagnostics.request_audio_snapshot()
                    os.startfile(str(folder))
                    result="Opened the local diagnostics folder."
                else:
                    diagnostics.clear()
                    result="Local logs cleared."
            except OSError:
                result="Windows couldn’t access the diagnostics folder."
            self.web.post_json(json.dumps({"type":"lummix-diagnostics","message":result}))
        elif command in ("startup-state","startup-on","startup-off") and source==NATIVE:
            from sign_in import SignIn
            startup = SignIn()
            try:
                state = startup.state() if command=="startup-state" else startup.change(command=="startup-on")
            except OSError:
                state = dict(enabled=None, editable=False, message="Lummix could not read or change Windows Startup apps.")
            self.web.post_json(json.dumps(dict(type="lummix-startup-state", **state)))

    def edge_size(self, hwnd):
        return self.native.lummix_frame_edge(hwnd, self.full)

    def resize(self):
        if self.web:
            _,_,width,height=gui.GetClientRect(self.hwnd)
            self.web.resize(width, height)

    def send_window_state(self, force=False):
        state=(bool(USER.IsZoomed(self.hwnd)),self.full)
        if self.web and (force or state!=self.last_window_state):
            try:
                self.web.post_json(json.dumps(dict(type="lummix-window-state",maximized=state[0],fullscreen=state[1])))
            except OSError:return
            self.last_window_state=state

    def remember(self):
        if not self.hwnd or self.full:return
        if not (USER.IsZoomed(self.hwnd) or gui.IsIconic(self.hwnd)):
            self.normal=gui.GetWindowRect(self.hwnd)
        rect=self.normal
        mon=win32api.MonitorFromRect(rect,2);info=win32api.GetMonitorInfo(mon)
        l,t,_,_=info["Work"];factor=96/monitor_dpi(mon)
        vals=[round(n*factor,2) for n in (rect[0]-l,rect[1]-t,rect[2]-rect[0],rect[3]-rect[1])]
        data=dict(zip(("x","y","width","height"),vals),screen=info["Device"],maximized=bool(USER.IsZoomed(self.hwnd)))
        file=local_dir()/"window-placement.json"
        try:
            profile_save.save(file, data)
        except OSError:pass

    def fullscreen(self):
        hwnd=self.hwnd
        if not self.full:
            self.remember();self.prev=gui.GetWindowPlacement(hwnd);self.style=gui.GetWindowLong(hwnd,wc.GWL_STYLE)
            self.full=True
            mon=win32api.MonitorFromWindow(hwnd,2);l,t,r,b=win32api.GetMonitorInfo(mon)["Monitor"]
            gui.SetWindowLong(hwnd,wc.GWL_STYLE,self.style & ~wc.WS_OVERLAPPEDWINDOW)
            gui.SetWindowPos(hwnd,0,l,t,r-l,b-t,wc.SWP_NOZORDER|wc.SWP_FRAMECHANGED)
        else:
            self.full=False;gui.SetWindowLong(hwnd,wc.GWL_STYLE,self.style)
            gui.SetWindowPlacement(hwnd,self.prev)
            gui.SetWindowPos(hwnd,0,0,0,0,0,wc.SWP_NOZORDER|wc.SWP_NOMOVE|wc.SWP_NOSIZE|wc.SWP_FRAMECHANGED)
        self.resize()
        self.send_window_state()

    def add_tray(self):
        self.tray = bool(self.native.lummix_tray_show(self.hwnd, self.icon, TRAY, self.tray))
        return self.tray

    def show(self):
        hidden=not gui.IsWindowVisible(self.hwnd)
        gui.ShowWindow(self.hwnd,wc.SW_RESTORE if gui.IsIconic(self.hwnd) else wc.SW_SHOW)
        if self.web:self.web.visible(True)
        try:gui.SetForegroundWindow(self.hwnd)
        except Exception:pass
        if hidden:self.resize()

    def popup(self):
        self.native.lummix_tray_menu(self.hwnd, OPEN, FULL, EXIT, *self.normal[:2])

    def quit(self):
        if self.exiting:return
        self.updates.cancelled.set()
        self.spotify_setup.cancelled.set()
        self.exiting=True;self.remember()
        if self.web and self.web.source().startswith("http://127.0.0.1:41835/"):
            self.exit_nonce=secrets.token_hex(12)
            script="""(async () => {
                try { await document.getElementById('canvas')?.contentWindow?.releaseLummixAudio?.(); }
                finally { chrome.webview.postMessage(%s); }
            })()""" % json.dumps("released:"+self.exit_nonce)
            try:
                if USER.SetTimer(self.hwnd, EXIT_TIMER, 3500, None):
                    self.web.execute(script)
                    return
            except OSError:pass
        self.finish_exit()

    def finish_exit(self):
        USER.KillTimer(self.hwnd,EXIT_TIMER)
        self.exit_nonce=None
        if self.tray:
            self.native.lummix_tray_hide(self.hwnd, self.icon, TRAY)
        if self.web is not None:self.web.close()
        gui.DestroyWindow(self.hwnd)

    def message(self,hwnd,msg,wp,lp):
        if msg in (wc.WM_ERASEBKGND,wc.WM_PRINTCLIENT):
            gui.FillRect(wp,gui.GetClientRect(hwnd),self.frame_brush)
            return 1
        if msg==wc.WM_PAINT:
            dc,paint=gui.BeginPaint(hwnd)
            try:gui.FillRect(dc,gui.GetClientRect(hwnd),self.frame_brush)
            finally:gui.EndPaint(hwnd,paint)
            return 0
        # Preserve the native resize/minimize/maximize/system menu styles so Windows
        # owns the resize loop, keyboard snapping and shell layout. The HTML owns the caption.
        if msg in (wc.WM_NCCALCSIZE, wc.WM_NCHITTEST, wc.WM_GETMINMAXINFO):
            return self.native.lummix_frame_message(hwnd, msg, ctypes.c_ssize_t(lp).value, self.full)
        if msg==wc.WM_COMMAND and wp in (MINIMIZE,MAXIMIZE,DRAG):
            if wp==MINIMIZE:gui.ShowWindow(hwnd,wc.SW_MINIMIZE)
            elif wp==MAXIMIZE:
                if self.full:self.fullscreen()
                else:gui.ShowWindow(hwnd,wc.SW_RESTORE if USER.IsZoomed(hwnd) else wc.SW_MAXIMIZE)
            elif not self.full and USER.GetAsyncKeyState(wc.VK_LBUTTON)&0x8000:
                USER.ReleaseCapture()
                gui.PostMessage(hwnd,wc.WM_NCLBUTTONDOWN,wc.HTCAPTION,USER.GetMessagePos())
            return 0
        if msg==wc.WM_COMMAND and wp==RESIZE:
            # WebView2 owns the client pixels, including the transparent resize targets.
            # Let Windows track the actual drag for snapping and multi-monitor DPI.
            if self.edge_size(hwnd) and lp in RESIZE_COMMANDS.values() and USER.GetAsyncKeyState(wc.VK_LBUTTON) & 0x8000:
                USER.ReleaseCapture()
                gui.PostMessage(hwnd, wc.WM_NCLBUTTONDOWN, lp, USER.GetMessagePos())
            return 0
        if msg==SPOTIFY_SETUP_CHANGED:
            if not self.exiting and self.web and self.web.source()==NATIVE:
                self.web.post_json(json.dumps(self.spotify_setup.status()))
            return 0
        if msg==UPDATE_CHANGED:
            if self.exiting: return 0
            state=self.updates.status()
            if state["phase"]=="launched": self.quit()
            elif self.web and self.web.source()==NATIVE:
                self.web.post_json(json.dumps(state))
            return 0
        if msg==CAN_REOPEN:return int(not self.exiting)
        if msg==FINISH_EXIT or (msg==wc.WM_TIMER and wp==EXIT_TIMER):
            if self.exiting:self.finish_exit()
            return 0
        if msg in (wc.WM_COMMAND,wc.WM_SYSCOMMAND) and wp in (OPEN,FULL,EXIT):
            if wp==OPEN and not self.exiting:self.show()
            elif wp==OPEN:return 0
            elif wp==FULL:self.show();self.fullscreen()
            else:self.quit()
            return 0
        if msg==wc.WM_CLOSE:
            if close_to_tray() and self.add_tray():
                self.remember()
                if self.web:self.web.visible(False)
                gui.ShowWindow(hwnd,wc.SW_HIDE)
            else:self.quit()
            return 0
        if msg==wc.WM_SIZE:
            if self.web:
                self.web.visible(wp!=wc.SIZE_MINIMIZED and bool(gui.IsWindowVisible(hwnd)))
                if wp!=wc.SIZE_MINIMIZED:self.resize()
                self.send_window_state()
            return 0
        if msg==wc.WM_MOVE and self.web:
            self.web.moved();return 0
        if msg==wc.WM_EXITSIZEMOVE:self.remember();return 0
        if msg==0x02e0 and self.hwnd: # WM_DPICHANGED recommended physical rectangle
            r=ctypes.cast(lp,ctypes.POINTER(wt.RECT)).contents
            gui.SetWindowPos(hwnd,0,r.left,r.top,r.right-r.left,r.bottom-r.top,wc.SWP_NOZORDER|wc.SWP_NOACTIVATE)
            self.resize();return 0
        if msg==wc.WM_KEYDOWN and (wp==wc.VK_F11 or (wp==wc.VK_ESCAPE and self.full)):
            if not (lp & (1<<30)):self.fullscreen()
            return 0
        if msg==TRAY:
            event=lp & 0xffff
            if event in (wc.WM_LBUTTONUP,wc.WM_LBUTTONDBLCLK,wc.WM_USER,wc.WM_USER+1):self.show()
            elif event in (wc.WM_RBUTTONUP,wc.WM_CONTEXTMENU):self.popup()
            return 0
        if msg==self.shell_restart and self.tray:
            self.tray=False
            if not self.add_tray() and not gui.IsWindowVisible(hwnd):self.show()
            return 0
        if msg==READY and wp:
            diagnostics.record("window_failed")
            gui.MessageBox(hwnd,self.native_error or "The studio could not initialize.","Lummix",wc.MB_ICONERROR)
            self.quit();return 0
        if msg==wc.WM_DESTROY:gui.PostQuitMessage(0);return 0
        if msg==wc.WM_ENDSESSION and wp:self.quit();return 0
        return gui.DefWindowProc(hwnd,msg,wp,lp)

def run():
    win=StudioWindow()
    try:win.start();gui.PumpMessages()
    finally:
        if gui.IsWindow(win.hwnd):win.quit()
