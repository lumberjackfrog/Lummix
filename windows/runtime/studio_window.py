"""Windows-owned studio window, notification icon and Microsoft WebView2 surface."""
import ctypes
from ctypes import wintypes as wt
import json
import os
from pathlib import Path
import secrets
import sys
from urllib.parse import urlsplit
from studio_assets import attach_installed_assets
import profile_save
import diagnostics
from client_update import Updates

import comtypes
from comtypes.gen import WebView2 as wv
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
EXIT_TIMER = 17
NATIVE = "http://127.0.0.1:41835/?view=desktop&app=1"
USER = ctypes.windll.user32
PROJECT = Path(__file__).resolve().parent.parent

class MinMaxInfo(ctypes.Structure):
    _fields_ = [(key, wt.POINT) for key in ("reserved", "maximum", "position", "minimum_track", "maximum_track")]

def resize_hit(rect, x, y, edge):
    left, top, right, bottom = rect
    horizontal = -1 if x < left+edge else 1 if x >= right-edge else 0
    vertical = -1 if y < top+edge else 1 if y >= bottom-edge else 0
    return ((wc.HTTOPLEFT,wc.HTTOP,wc.HTTOPRIGHT),
            (wc.HTLEFT,wc.HTCLIENT,wc.HTRIGHT),
            (wc.HTBOTTOMLEFT,wc.HTBOTTOM,wc.HTBOTTOMRIGHT))[vertical+1][horizontal+1]

class NotifyIcon(ctypes.Structure):
    _fields_=[("size",wt.DWORD),("window",wt.HWND),("id",wt.UINT),("flags",wt.UINT),
        ("callback",wt.UINT),("icon",wt.HICON),("tip",wt.WCHAR*128),("state",wt.DWORD),
        ("stateMask",wt.DWORD),("info",wt.WCHAR*256),("version",wt.UINT),("infoTitle",wt.WCHAR*64),
        ("infoFlags",wt.DWORD),("guid",ctypes.c_byte*16),("balloon",wt.HICON)]

def notify(operation,hwnd,icon):
    data=NotifyIcon()
    data.size=ctypes.sizeof(data);data.window=hwnd;data.id=1
    data.flags=gui.NIF_ICON|gui.NIF_TIP|gui.NIF_MESSAGE|0x80
    data.callback=TRAY;data.icon=icon;data.tip="Lummix";data.version=4
    return bool(ctypes.windll.shell32.Shell_NotifyIconW(operation,ctypes.byref(data)))

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
        self.hwnd=0;self.tray=False;self.full=False;self.exiting=False
        self.exit_nonce=None
        self.controller=self.web=None
        self.last_window_state=None
        self.callbacks=[];self.native_error=None
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

    def bind(self,interface,fn):
        host=self
        class Event(comtypes.COMObject):
            _com_interfaces_=[interface]
            def Invoke(self,*args):
                try:return fn(*args)
                except Exception as exc:
                    host.native_error=str(exc)
                    gui.PostMessage(host.hwnd,READY,1,0)
                    return 0
        obj=Event();self.callbacks.append(obj)
        return obj.QueryInterface(interface)

    def start(self):
        self.loader=ctypes.WinDLL(str(data_file("WebView2Loader.dll")))
        create=self.loader.CreateCoreWebView2EnvironmentWithOptions
        iface=wv.ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler
        create.argtypes=[wt.LPCWSTR,wt.LPCWSTR,ctypes.c_void_p,ctypes.POINTER(iface)]
        create.restype=ctypes.c_long
        # WebView2 uses an app-owned profile. No personal browser profile is accessed.
        path=local_dir()/"Studio WebView"
        result=create(None,str(path),None,self.bind(iface,self.environment_ready))
        if result<0: raise RuntimeError("Microsoft Edge WebView2 Runtime could not start. Install the Evergreen x64 runtime from Microsoft and reopen Lummix.")

    def environment_ready(self,code,env):
        if code<0 or not env:raise RuntimeError("Microsoft Edge WebView2 Runtime could not initialize.")
        self.environment=env.QueryInterface(wv.ICoreWebView2Environment)
        self.environment.CreateCoreWebView2Controller(self.hwnd,self.bind(
            wv.ICoreWebView2CreateCoreWebView2ControllerCompletedHandler,self.controller_ready))
        return 0

    def controller_ready(self,code,controller):
        if code<0 or not controller:raise RuntimeError("Microsoft Edge WebView2 could not open the studio.")
        self.controller=controller.QueryInterface(wv.ICoreWebView2Controller)
        self.web=self.controller.CoreWebView2
        self.web.Settings.AreHostObjectsAllowed=False
        try:self.web.Settings.QueryInterface(wv.ICoreWebView2Settings9).IsNonClientRegionSupportEnabled=True
        except comtypes.COMError:pass  # Older runtimes use the header's pointer and double-click bridge.
        self.web.add_WebMessageReceived(self.bind(wv.ICoreWebView2WebMessageReceivedEventHandler,self.web_message))
        self.web.add_NewWindowRequested(self.bind(wv.ICoreWebView2NewWindowRequestedEventHandler,self.new_page))
        self.web.add_NavigationStarting(self.bind(wv.ICoreWebView2NavigationStartingEventHandler,self.navigation))
        self.controller.add_AcceleratorKeyPressed(self.bind(wv.ICoreWebView2AcceleratorKeyPressedEventHandler,self.accelerator))
        studio_root = Path(sys.executable).parent if getattr(sys, "frozen", False) else PROJECT.parent
        attach_installed_assets(self, studio_root / "studio")
        self.resize();self.web.Navigate(NATIVE)
        self.updates.command("auto")
        return 0

    def update_changed(self):
        if self.hwnd and not self.exiting:
            try: gui.PostMessage(self.hwnd,UPDATE_CHANGED,0,0)
            except OSError: pass

    def navigation(self,sender,args):
        if urlsplit(args.Uri).netloc!="127.0.0.1:41835" or not args.Uri.startswith("http://127.0.0.1:41835/"):
            args.Cancel=True
        return 0

    def new_page(self,sender,args):
        args.Handled=True
        uri=args.Uri
        if args.IsUserInitiated and (uri=="spotify:" or (urlsplit(uri).scheme=="https" and
            urlsplit(uri).hostname in {"vb-audio.com","www.vb-audio.com","www.spotify.com","open.spotify.com"})):
            try:os.startfile(uri)
            except OSError:pass
        return 0

    def web_message(self,sender,args):
        if not args.Source.startswith("http://127.0.0.1:41835/"):return 0
        try:command=args.TryGetWebMessageAsString()
        except comtypes.COMError:return 0
        if self.exit_nonce and command=="released:"+self.exit_nonce:
            gui.PostMessage(self.hwnd,FINISH_EXIT,0,0)
        elif command in WINDOW_COMMANDS:gui.PostMessage(self.hwnd,wc.WM_COMMAND,WINDOW_COMMANDS[command],0)
        elif command in RESIZE_COMMANDS:gui.PostMessage(self.hwnd,wc.WM_COMMAND,RESIZE,RESIZE_COMMANDS[command])
        elif command=="close":gui.PostMessage(self.hwnd,wc.WM_CLOSE,0,0)
        elif command=="window-state":self.send_window_state(force=True)
        elif command in ("update-state","update-check","update-download","update-install") and args.Source==NATIVE and not self.exiting:
            if command!="update-state":
                self.updates.command(command.removeprefix("update-"))
            self.web.PostWebMessageAsJson(json.dumps(self.updates.status()))
        elif command in ("logs-open","logs-clear") and args.Source==NATIVE:
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
            self.web.PostWebMessageAsJson(json.dumps({"type":"lummix-diagnostics","message":result}))
        elif command in ("startup-state","startup-on","startup-off") and args.Source==NATIVE:
            from sign_in import SignIn
            startup = SignIn()
            try:
                state = startup.state() if command=="startup-state" else startup.change(command=="startup-on")
            except OSError:
                state = dict(enabled=None, editable=False, message="Lummix could not read or change Windows Startup apps.")
            self.web.PostWebMessageAsJson(json.dumps(dict(type="lummix-startup-state", **state)))
        return 0

    def accelerator(self,sender,args):
        if args.KeyEventKind in (0,2) and args.VirtualKey in (wc.VK_F11,wc.VK_ESCAPE):
            if args.VirtualKey==wc.VK_F11 or self.full:
                args.Handled=True
                if not args.PhysicalKeyStatus.WasKeyDown:gui.PostMessage(self.hwnd,wc.WM_COMMAND,FULL,0)
        return 0

    def edge_size(self, hwnd):
        if self.full or USER.IsZoomed(hwnd):return 0
        return max(3,round(5 * (USER.GetDpiForWindow(hwnd) or 96) / 96))

    def resize(self):
        if self.controller:
            _,_,width,height=gui.GetClientRect(self.hwnd)
            self.controller.Bounds=wt.RECT(0,0,width,height)

    def send_window_state(self, force=False):
        state=(bool(USER.IsZoomed(self.hwnd)),self.full)
        if self.web and (force or state!=self.last_window_state):
            try:
                self.web.PostWebMessageAsJson(json.dumps(dict(type="lummix-window-state",maximized=state[0],fullscreen=state[1])))
            except comtypes.COMError:return
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
        if self.tray and notify(gui.NIM_MODIFY,self.hwnd,self.icon):return True
        self.tray=notify(gui.NIM_ADD,self.hwnd,self.icon)
        if self.tray:notify(4,self.hwnd,self.icon) # NIM_SETVERSION, NOTIFYICON_VERSION_4
        return self.tray

    def show(self):
        hidden=not gui.IsWindowVisible(self.hwnd)
        gui.ShowWindow(self.hwnd,wc.SW_RESTORE if gui.IsIconic(self.hwnd) else wc.SW_SHOW)
        if self.controller:self.controller.IsVisible=True
        try:gui.SetForegroundWindow(self.hwnd)
        except Exception:pass
        if hidden:self.resize()

    def popup(self):
        menu=gui.CreatePopupMenu()
        try:
            for cmd,label in ((OPEN,"Open Lummix"),(FULL,"Full screen"),(EXIT,"Exit Lummix")):
                if cmd==EXIT:gui.AppendMenu(menu,wc.MF_SEPARATOR,0,"")
                gui.AppendMenu(menu,wc.MF_STRING,cmd,label)
            try:gui.SetForegroundWindow(self.hwnd)
            except Exception:pass
            try:x,y=gui.GetCursorPos()
            except Exception:x,y=self.normal[:2]
            cmd=gui.TrackPopupMenu(menu,wc.TPM_RETURNCMD|wc.TPM_RIGHTBUTTON,x,y,0,self.hwnd,None)
            if cmd:gui.PostMessage(self.hwnd,wc.WM_COMMAND,cmd,0)
        finally:gui.DestroyMenu(menu)

    def quit(self):
        if self.exiting:return
        self.updates.cancelled.set()
        self.exiting=True;self.remember()
        if self.web and self.web.Source.startswith("http://127.0.0.1:41835/"):
            self.exit_nonce=secrets.token_hex(12)
            script="""(async () => {
                try { await document.getElementById('canvas')?.contentWindow?.releaseLummixAudio?.(); }
                finally { chrome.webview.postMessage(%s); }
            })()""" % json.dumps("released:"+self.exit_nonce)
            try:
                if USER.SetTimer(self.hwnd, EXIT_TIMER, 3500, None):
                    self.web.ExecuteScript(script,None)
                    return
            except (OSError,comtypes.COMError):pass
        self.finish_exit()

    def finish_exit(self):
        USER.KillTimer(self.hwnd,EXIT_TIMER)
        self.exit_nonce=None
        if self.tray:
            notify(gui.NIM_DELETE,self.hwnd,self.icon)
        if self.controller:self.controller.Close()
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
        if msg==wc.WM_NCCALCSIZE:
            if USER.IsZoomed(hwnd) and not self.full:
                # Windows extends a thick frame beyond the work area on maximize.
                # Keep the entire visible WebView inside the real monitor work area.
                rect=ctypes.cast(lp,ctypes.POINTER(wt.RECT)).contents
                proposed=rect.left,rect.top,rect.right,rect.bottom
                work=win32api.GetMonitorInfo(win32api.MonitorFromRect(proposed,2))["Work"]
                rect.left,rect.top=max(rect.left,work[0]),max(rect.top,work[1])
                rect.right,rect.bottom=min(rect.right,work[2]),min(rect.bottom,work[3])
            return 0
        if msg==wc.WM_NCHITTEST:
            edge=self.edge_size(hwnd)
            if edge:
                return resize_hit(gui.GetWindowRect(hwnd),ctypes.c_short(lp & 0xffff).value,
                                  ctypes.c_short((lp >> 16) & 0xffff).value,edge)
            return wc.HTCLIENT
        if msg==wc.WM_GETMINMAXINFO:
            info=ctypes.cast(lp,ctypes.POINTER(MinMaxInfo)).contents
            monitor=win32api.GetMonitorInfo(win32api.MonitorFromWindow(hwnd,2))
            ml,mt,_,_=monitor["Monitor"]
            left,top,right,bottom=monitor["Monitor" if self.full else "Work"]
            info.position.x,info.position.y=left-ml,top-mt
            info.maximum.x,info.maximum.y=right-left,bottom-top
            dpi=USER.GetDpiForWindow(hwnd) or 96
            info.minimum_track.x,info.minimum_track.y=round(320*dpi/96),round(360*dpi/96)
            return 0
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
        if msg==UPDATE_CHANGED:
            if self.exiting: return 0
            state=self.updates.status()
            if state["phase"]=="launched": self.quit()
            elif self.web and self.web.Source==NATIVE:
                self.web.PostWebMessageAsJson(json.dumps(state))
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
                if self.controller:self.controller.IsVisible=False
                gui.ShowWindow(hwnd,wc.SW_HIDE)
            else:self.quit()
            return 0
        if msg==wc.WM_SIZE:
            if self.controller:
                self.controller.IsVisible=wp!=wc.SIZE_MINIMIZED and bool(gui.IsWindowVisible(hwnd))
                if wp!=wc.SIZE_MINIMIZED:self.resize()
                self.send_window_state()
            return 0
        if msg==wc.WM_MOVE and self.controller:
            self.controller.NotifyParentWindowPositionChanged();return 0
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
