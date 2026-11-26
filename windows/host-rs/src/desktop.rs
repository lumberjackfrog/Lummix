use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::core::{w, PCWSTR};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, EnumDisplayMonitors, FillRect,
    GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, MonitorFromWindow, HBRUSH, HDC, HMONITOR,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, ReleaseCapture};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::{webview, window};

pub(crate) const CLASS: PCWSTR = w!("LumMixStudioHost");
const CAPTION: PCWSTR = w!("Lummix");
const ORIGIN: &str = "http://127.0.0.1:41835/";
pub(crate) const OPEN: usize = 0x7120;
const FULL: usize = 0x7130;
const EXIT: usize = 0x7140;
const MINIMIZE: usize = 0x7150;
const MAXIMIZE: usize = 0x7160;
const DRAG: usize = 0x7170;
const RESIZE: usize = 0x7180;
const TRAY: u32 = WM_APP + 290;
const FAILURE: u32 = WM_APP + 291;
const FINISH: u32 = WM_APP + 292;
pub(crate) const CAN_REOPEN: u32 = WM_APP + 293;
const UPDATE_CHANGED: u32 = WM_APP + 294;
const SPOTIFY_CHANGED: u32 = WM_APP + 295;
const EXIT_TIMER: usize = 17;

pub(crate) const CLOSE_TO_TRAY: u32 = 0;
pub(crate) const REMEMBER: u32 = 1;
pub(crate) const BEGIN_EXIT: u32 = 2;
pub(crate) const FINISH_EXIT: u32 = 3;
pub(crate) const UPDATE: u32 = 4;
pub(crate) const SPOTIFY: u32 = 5;
pub(crate) const WINDOW_FAILED: u32 = 6;

type Event = extern "C" fn(u32, *const u8, usize) -> i32;

#[repr(C)]
pub(crate) struct SavedPlacement {
    pub geometry: [f64; 4],
    pub screen: [u16; 32],
    pub maximized: i32,
}

struct Desktop {
    hwnd: Cell<HWND>,
    web: Cell<*const c_void>,
    callback: Event,
    icon: HICON,
    small_icon: HICON,
    background: HBRUSH,
    normal: Cell<RECT>,
    previous: Cell<Option<(WINDOWPLACEMENT, i32)>>,
    last_web_state: Cell<Option<(bool, bool)>>,
    shell_restart: u32,
    tray: Cell<bool>,
    exiting: Cell<bool>,
    exit_nonce: RefCell<Option<String>>,
    failure: RefCell<Option<Vec<u16>>>,
    pumping: Cell<bool>,
    registered: bool,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn monitor_info(monitor: HMONITOR) -> Option<MONITORINFOEXW> {
    let mut info: MONITORINFOEXW = unsafe { zeroed() };
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    (unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) } != 0).then_some(info)
}

fn monitor_dpi(monitor: HMONITOR) -> f64 {
    let (mut x, mut y) = (96, 96);
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) } < 0 || x == 0 {
        x = 96;
    }
    f64::from(x)
}

fn terminated(wide: &[u16]) -> &[u16] {
    &wide[..wide
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(wide.len())]
}

fn locate_monitor(screen: &[u16]) -> HMONITOR {
    struct Find<'a> {
        wanted: &'a [u16],
        result: HMONITOR,
    }
    unsafe extern "system" fn find(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> i32 {
        let search = &mut *(data as *mut Find<'_>);
        if monitor_info(monitor).is_some_and(|info| terminated(&info.szDevice) == search.wanted) {
            search.result = monitor;
            return 0;
        }
        1
    }
    let mut search = Find {
        wanted: screen,
        result: null_mut(),
    };
    unsafe {
        if !screen.is_empty() {
            EnumDisplayMonitors(
                null_mut(),
                null(),
                Some(find),
                (&mut search as *mut Find<'_>) as LPARAM,
            );
        }
        if search.result.is_null() {
            let mut point = POINT { x: 0, y: 0 };
            GetCursorPos(&mut point);
            search.result = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        }
    }
    search.result
}

fn placement(values: &[f64], area: RECT, dpi: f64) -> [i32; 4] {
    let factor = dpi / 96.0;
    let value = |index: usize, default: i32| {
        values
            .get(index)
            .filter(|value| value.abs() < 20000.0)
            .map_or(default, |value| (value * factor).round_ties_even() as i32)
    };
    let outer_w = area.right - area.left;
    let outer_h = area.bottom - area.top;
    let width = outer_w.min(value(2, (1280.0 * factor).round_ties_even() as i32).max(330));
    let height = outer_h.min(value(3, (830.0 * factor).round_ties_even() as i32).max(360));
    let x = (area.left + value(0, (outer_w - width) / 2)).clamp(area.left, area.right - width);
    let y = (area.top + value(1, (outer_h - height) / 2)).clamp(area.top, area.bottom - height);
    [x, y, width, height]
}

impl Desktop {
    fn full(&self) -> bool {
        self.previous.get().is_some()
    }

    fn emit(&self, event: u32, data: &[u8]) -> bool {
        (self.callback)(event, data.as_ptr(), data.len()) != 0
    }

    fn remember(&self) {
        let hwnd = self.hwnd.get();
        if hwnd.is_null() || self.full() {
            return;
        }
        let zoom = unsafe { IsZoomed(hwnd) } != 0;
        if !zoom && unsafe { IsIconic(hwnd) } == 0 {
            let mut normal = self.normal.get();
            if unsafe { GetWindowRect(hwnd, &mut normal) } != 0 {
                self.normal.set(normal);
            }
        }
        let normal = self.normal.get();
        let monitor = unsafe { MonitorFromRect(&normal, MONITOR_DEFAULTTONEAREST) };
        let Some(info) = monitor_info(monitor) else {
            return;
        };
        let area = info.monitorInfo.rcWork;
        let factor = 96.0 / monitor_dpi(monitor);
        let logical = |value| (f64::from(value) * factor * 100.0).round_ties_even() / 100.0;
        let data = SavedPlacement {
            geometry: [
                logical(normal.left - area.left),
                logical(normal.top - area.top),
                logical(normal.right - normal.left),
                logical(normal.bottom - normal.top),
            ],
            screen: info.szDevice,
            maximized: zoom.into(),
        };
        (self.callback)(
            REMEMBER,
            (&data as *const SavedPlacement).cast(),
            size_of::<SavedPlacement>(),
        );
    }

    fn send_window_state(&self, force: bool) {
        let web = self.web.get();
        if web.is_null() {
            return;
        }
        let state = (unsafe { IsZoomed(self.hwnd.get()) } != 0, self.full());
        if force || self.last_web_state.get() != Some(state) {
            let content = wide(&format!(
                r#"{{"type":"lummix-window-state","maximized":{},"fullscreen":{}}}"#,
                state.0, state.1
            ));
            if unsafe { webview::lummix_webview_post_json(web, content.as_ptr()) } >= 0 {
                self.last_web_state.set(Some(state));
            }
        }
    }

    fn resize(&self) {
        let web = self.web.get();
        if !web.is_null() {
            let mut rect: RECT = unsafe { zeroed() };
            if unsafe { GetClientRect(self.hwnd.get(), &mut rect) } != 0 {
                unsafe { webview::lummix_webview_resize(web, rect.right, rect.bottom) };
            }
        }
    }

    fn visible(&self, visible: bool) {
        let web = self.web.get();
        if !web.is_null() {
            unsafe { webview::lummix_webview_visible(web, i32::from(visible)) };
        }
    }

    fn show(&self) {
        let hwnd = self.hwnd.get();
        let hidden = unsafe { IsWindowVisible(hwnd) } == 0;
        unsafe {
            ShowWindow(
                hwnd,
                if IsIconic(hwnd) != 0 {
                    SW_RESTORE
                } else {
                    SW_SHOW
                },
            )
        };
        self.visible(true);
        unsafe { SetForegroundWindow(hwnd) };
        if hidden {
            self.resize();
        }
    }

    fn toggle_full(&self) {
        let hwnd = self.hwnd.get();
        unsafe {
            if let Some((previous, style)) = self.previous.replace(None) {
                SetWindowLongW(hwnd, GWL_STYLE, style);
                SetWindowPlacement(hwnd, &previous);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOZORDER | SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED,
                );
            } else {
                let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                let Some(info) = monitor_info(monitor) else {
                    return;
                };
                let mut previous: WINDOWPLACEMENT = zeroed();
                previous.length = size_of::<WINDOWPLACEMENT>() as u32;
                if GetWindowPlacement(hwnd, &mut previous) == 0 {
                    return;
                }
                self.remember();
                let style = GetWindowLongW(hwnd, GWL_STYLE);
                self.previous.set(Some((previous, style)));
                let area = info.monitorInfo.rcMonitor;
                SetWindowLongW(hwnd, GWL_STYLE, style & !(WS_OVERLAPPEDWINDOW as i32));
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    area.left,
                    area.top,
                    area.right - area.left,
                    area.bottom - area.top,
                    SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            }
        }
        self.resize();
        self.send_window_state(false);
    }

    fn add_tray(&self) -> bool {
        let added =
            window::lummix_tray_show(self.hwnd.get(), self.icon, TRAY, self.tray.get().into()) != 0;
        self.tray.set(added);
        added
    }

    fn quit(&self) {
        if self.exiting.replace(true) {
            return;
        }
        self.emit(BEGIN_EXIT, &[]);
        self.remember();
        let web = self.web.get();
        if unsafe { webview::document_source(web) }.is_some_and(|uri| uri.starts_with(ORIGIN)) {
            let mut bytes = [0u8; 12];
            let status = unsafe {
                BCryptGenRandom(
                    null_mut(),
                    bytes.as_mut_ptr(),
                    bytes.len() as u32,
                    BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                )
            };
            if status == 0 {
                let nonce = String::from("released:")
                    + &bytes
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                let script = wide(&format!(
                    "(async () => {{ try {{ await document.getElementById('canvas')?.contentWindow?.releaseLummixAudio?.(); }} finally {{ chrome.webview.postMessage('{nonce}'); }} }})()"
                ));
                *self.exit_nonce.borrow_mut() = Some(nonce);
                if unsafe { SetTimer(self.hwnd.get(), EXIT_TIMER, 3500, None) } != 0
                    && unsafe { webview::lummix_webview_execute(web, script.as_ptr()) } >= 0
                {
                    return;
                }
            }
        }
        self.finish();
    }

    fn finish(&self) {
        let hwnd = self.hwnd.get();
        if hwnd.is_null() {
            return;
        }
        unsafe { KillTimer(hwnd, EXIT_TIMER) };
        *self.exit_nonce.borrow_mut() = None;
        if self.tray.replace(false) {
            window::lummix_tray_hide(hwnd, self.icon, TRAY);
        }
        self.web.set(null());
        self.emit(FINISH_EXIT, &[]);
        unsafe { DestroyWindow(hwnd) };
    }

    fn post(&self, message: u32, wp: WPARAM, lp: LPARAM) {
        unsafe { PostMessageW(self.hwnd.get(), message, wp, lp) };
    }

    fn drag(&self, hit: usize) {
        let hwnd = self.hwnd.get();
        if unsafe { GetAsyncKeyState(1) } < 0 {
            unsafe {
                ReleaseCapture();
                PostMessageW(hwnd, WM_NCLBUTTONDOWN, hit, GetMessagePos() as LPARAM);
            }
        }
    }

    unsafe fn message(&self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_ERASEBKGND | WM_PRINTCLIENT => {
                let mut rect: RECT = zeroed();
                GetClientRect(hwnd, &mut rect);
                FillRect(wp as HDC, &rect, self.background);
                return 1;
            }
            WM_PAINT => {
                let mut paint: PAINTSTRUCT = zeroed();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut rect: RECT = zeroed();
                GetClientRect(hwnd, &mut rect);
                FillRect(dc, &rect, self.background);
                EndPaint(hwnd, &paint);
            }
            WM_NCCALCSIZE | WM_NCHITTEST | WM_GETMINMAXINFO => {
                return window::lummix_frame_message(hwnd, msg, lp, self.full().into());
            }
            WM_COMMAND if wp == MINIMIZE => {
                ShowWindow(hwnd, SW_MINIMIZE);
            }
            WM_COMMAND if wp == MAXIMIZE => {
                if self.full() {
                    self.toggle_full();
                } else {
                    ShowWindow(
                        hwnd,
                        if IsZoomed(hwnd) != 0 {
                            SW_RESTORE
                        } else {
                            SW_MAXIMIZE
                        },
                    );
                }
            }
            WM_COMMAND if wp == DRAG => {
                if !self.full() {
                    self.drag(HTCAPTION as usize);
                }
            }
            WM_COMMAND if wp == RESIZE => {
                if window::lummix_frame_edge(hwnd, self.full().into()) != 0
                    && (10..=17).contains(&lp)
                {
                    self.drag(lp as usize);
                }
            }
            WM_COMMAND | WM_SYSCOMMAND if wp == OPEN => {
                if !self.exiting.get() {
                    self.show();
                }
            }
            WM_COMMAND | WM_SYSCOMMAND if wp == FULL => {
                self.show();
                self.toggle_full();
            }
            WM_COMMAND | WM_SYSCOMMAND if wp == EXIT => self.quit(),
            CAN_REOPEN => return isize::from(!self.exiting.get()),
            FINISH => {
                if self.exiting.get() {
                    self.finish();
                }
            }
            WM_TIMER if wp == EXIT_TIMER => {
                if self.exiting.get() {
                    self.finish();
                }
            }
            WM_CLOSE => {
                if self.emit(CLOSE_TO_TRAY, &[]) && self.add_tray() {
                    self.remember();
                    self.visible(false);
                    ShowWindow(hwnd, SW_HIDE);
                } else {
                    self.quit();
                }
            }
            WM_SIZE => {
                self.visible(wp != SIZE_MINIMIZED as usize && IsWindowVisible(hwnd) != 0);
                if wp != SIZE_MINIMIZED as usize {
                    self.resize();
                }
                self.send_window_state(false);
            }
            WM_MOVE if !self.web.get().is_null() => {
                webview::lummix_webview_moved(self.web.get());
            }
            WM_EXITSIZEMOVE => self.remember(),
            WM_DPICHANGED if !self.hwnd.get().is_null() && lp != 0 => {
                let rect = &*(lp as *const RECT);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                self.resize();
            }
            WM_KEYDOWN if wp == 0x7a || (wp == 0x1b && self.full()) => {
                if lp & (1 << 30) == 0 {
                    self.toggle_full();
                }
            }
            TRAY => match (lp & 0xffff) as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK | 0x400 | 0x401 => self.show(),
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let rect = self.normal.get();
                    window::lummix_tray_menu(hwnd, OPEN, FULL, EXIT, rect.left, rect.top);
                }
                _ => {}
            },
            message if message == self.shell_restart && self.tray.get() => {
                self.tray.set(false);
                if !self.add_tray() && IsWindowVisible(hwnd) == 0 {
                    self.show();
                }
            }
            FAILURE => {
                self.emit(WINDOW_FAILED, &[]);
                let message = self.failure.borrow_mut().take();
                MessageBoxW(
                    hwnd,
                    message
                        .as_ref()
                        .map_or(w!("The studio could not initialize."), Vec::as_ptr),
                    CAPTION,
                    MB_ICONERROR,
                );
                self.quit();
            }
            UPDATE_CHANGED => {
                if !self.exiting.get() {
                    self.emit(UPDATE, &[]);
                }
            }
            SPOTIFY_CHANGED => {
                if !self.exiting.get() {
                    self.emit(SPOTIFY, &[]);
                }
            }
            WM_DESTROY => {
                if self.pumping.get() {
                    PostQuitMessage(0);
                }
            }
            WM_ENDSESSION if wp != 0 => self.quit(),
            _ => return DefWindowProcW(hwnd, msg, wp, lp),
        }
        0
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        if lp == 0 {
            return 0;
        }
        let create = &*(lp as *const CREATESTRUCTW);
        let desktop = create.lpCreateParams.cast::<Desktop>();
        if desktop.is_null() {
            return 0;
        }
        (*desktop).hwnd.set(hwnd);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, desktop as isize);
    }
    let desktop = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Desktop).as_ref();
    if let Some(desktop) = desktop {
        if msg == WM_NCDESTROY {
            desktop.hwnd.set(null_mut());
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        } else {
            return desktop.message(hwnd, msg, wp, lp);
        }
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.finish();
        unsafe {
            if self.registered {
                UnregisterClassW(CLASS, GetModuleHandleW(null()));
            }
            DeleteObject(self.background);
            DestroyIcon(self.icon);
            DestroyIcon(self.small_icon);
        }
    }
}

/// UI-thread calls supply stable UTF-16 strings, at most four placement values, and a callback
/// that remains callable until the window token is closed.
#[no_mangle]
pub unsafe extern "C" fn lummix_desktop_create(
    icon_path: PCWSTR,
    screen: PCWSTR,
    saved: *const f64,
    callback: Option<Event>,
) -> *mut c_void {
    let Some(callback) = callback else {
        return null_mut();
    };
    if icon_path.is_null() {
        return null_mut();
    }
    let screen_len = if screen.is_null() {
        0
    } else {
        (0..).take_while(|&i| *screen.add(i) != 0).count()
    };
    let screen = if screen_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(screen, screen_len)
    };
    let monitor = locate_monitor(screen);
    let Some(info) = monitor_info(monitor) else {
        return null_mut();
    };
    let values = if saved.is_null() {
        &[]
    } else {
        std::slice::from_raw_parts(saved, 4)
    };
    let [x, y, width, height] = placement(values, info.monitorInfo.rcWork, monitor_dpi(monitor));
    let icon = LoadImageW(null_mut(), icon_path, IMAGE_ICON, 32, 32, LR_LOADFROMFILE);
    let small_icon = LoadImageW(null_mut(), icon_path, IMAGE_ICON, 16, 16, LR_LOADFROMFILE);
    let background = CreateSolidBrush(24 | (22 << 8) | (26 << 16));
    let instance = GetModuleHandleW(null());
    let style = WS_POPUP | (WS_OVERLAPPEDWINDOW & !WS_CAPTION) | WS_CLIPCHILDREN;
    let mut desktop = Box::new(Desktop {
        hwnd: Cell::new(null_mut()),
        web: Cell::new(null()),
        callback,
        icon,
        small_icon,
        background,
        normal: Cell::new(RECT {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        }),
        previous: Cell::new(None),
        last_web_state: Cell::new(None),
        shell_restart: RegisterWindowMessageW(w!("TaskbarCreated")),
        tray: Cell::new(false),
        exiting: Cell::new(false),
        exit_nonce: RefCell::new(None),
        failure: RefCell::new(None),
        pumping: Cell::new(false),
        registered: false,
    });
    let class = WNDCLASSW {
        style: CS_DBLCLKS,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: icon,
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: background,
        lpszMenuName: null(),
        lpszClassName: CLASS,
    };
    if RegisterClassW(&class) == 0 {
        return null_mut();
    }
    desktop.registered = true;
    let hwnd = CreateWindowExW(
        0,
        CLASS,
        CAPTION,
        style,
        x,
        y,
        width,
        height,
        null_mut(),
        null_mut(),
        instance,
        (&mut *desktop as *mut Desktop).cast(),
    );
    if hwnd.is_null() {
        return null_mut();
    }
    SetWindowLongPtrW(
        hwnd,
        GWLP_USERDATA,
        (&mut *desktop as *mut Desktop) as isize,
    );
    SendMessageW(hwnd, WM_SETICON, 0, small_icon as isize);
    let menu = GetSystemMenu(hwnd, 0);
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, FULL, w!("Full screen\tF11"));
    AppendMenuW(menu, MF_STRING, EXIT, w!("Exit Lummix"));
    Box::into_raw(desktop).cast()
}

unsafe fn desktop<'a>(token: *const c_void) -> Option<&'a Desktop> {
    token.cast::<Desktop>().as_ref()
}

#[no_mangle]
pub unsafe extern "C" fn lummix_desktop_hwnd(token: *const c_void) -> HWND {
    desktop(token).map_or(null_mut(), |desktop| desktop.hwnd.get())
}

#[no_mangle]
pub unsafe extern "C" fn lummix_desktop_show(token: *const c_void, maxed: i32, no_outline: i32) {
    if let Some(desktop) = desktop(token) {
        let hwnd = desktop.hwnd.get();
        if no_outline != 0 {
            let no_border = 0xfffffffeu32;
            DwmSetWindowAttribute(
                hwnd,
                34,
                (&no_border as *const u32).cast(),
                size_of::<u32>() as u32,
            );
        }
        use windows_sys::Win32::System::Threading::{
            GetStartupInfoW, STARTF_USESHOWWINDOW, STARTUPINFOW,
        };
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        GetStartupInfoW(&mut startup);
        let restore = if maxed != 0 {
            SW_SHOWMAXIMIZED
        } else {
            SW_SHOWNORMAL
        };
        let show = match (
            startup.dwFlags & STARTF_USESHOWWINDOW != 0,
            startup.wShowWindow as i32,
        ) {
            (
                true,
                explicit @ (SW_HIDE | SW_SHOWMINIMIZED | SW_MINIMIZE | SW_SHOWMINNOACTIVE
                | SW_SHOWNOACTIVATE | SW_SHOWMAXIMIZED),
            ) => explicit,
            _ => restore,
        };
        ShowWindow(hwnd, show);
        windows_sys::Win32::Graphics::Gdi::UpdateWindow(hwnd);
    }
}

#[no_mangle]
pub unsafe extern "C" fn lummix_desktop_attach_web(token: *const c_void, web: *const c_void) {
    if let Some(desktop) = desktop(token) {
        desktop.web.set(web);
    }
}

#[no_mangle]
pub unsafe extern "C" fn lummix_desktop_pump(token: *const c_void) -> i32 {
    let Some(desktop) = desktop(token).filter(|desktop| !desktop.hwnd.get().is_null()) else {
        return 0;
    };
    desktop.pumping.set(true);
    let mut msg: MSG = zeroed();
    let result = loop {
        match GetMessageW(&mut msg, null_mut(), 0, 0) {
            -1 => break -1,
            0 => break 0,
            _ => {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    };
    desktop.pumping.set(false);
    result
}

#[no_mangle]
pub unsafe extern "C" fn lummix_desktop_close(token: *mut c_void) {
    if !token.is_null() {
        drop(Box::from_raw(token.cast::<Desktop>()));
    }
}

#[no_mangle]
pub unsafe extern "C" fn lummix_desktop_command(
    token: *const c_void,
    command: PCWSTR,
    length: usize,
) -> i32 {
    let Some(desktop) = desktop(token) else {
        return 0;
    };
    if command.is_null() || length > 128 {
        return 0;
    }
    let command = String::from_utf16_lossy(std::slice::from_raw_parts(command, length));
    if desktop.exit_nonce.borrow().as_deref() == Some(command.as_str()) {
        desktop.post(FINISH, 0, 0);
        return 1;
    }
    let action = match command.as_str() {
        "fullscreen" => FULL,
        "exit" => EXIT,
        "minimize" => MINIMIZE,
        "maximize" => MAXIMIZE,
        "drag" => DRAG,
        "resize-nw" => HTTOPLEFT as usize,
        "resize-n" => HTTOP as usize,
        "resize-ne" => HTTOPRIGHT as usize,
        "resize-w" => HTLEFT as usize,
        "resize-e" => HTRIGHT as usize,
        "resize-sw" => HTBOTTOMLEFT as usize,
        "resize-s" => HTBOTTOM as usize,
        "resize-se" => HTBOTTOMRIGHT as usize,
        "close" => {
            desktop.post(WM_CLOSE, 0, 0);
            return 1;
        }
        "window-state" => {
            desktop.send_window_state(true);
            return 1;
        }
        _ => return 0,
    };
    if action < RESIZE && action >= OPEN {
        desktop.post(WM_COMMAND, action, 0);
    } else {
        desktop.post(WM_COMMAND, RESIZE, action as LPARAM);
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn lummix_desktop_accelerator(token: *const c_void, detail: u32) -> i32 {
    let Some(desktop) = desktop(token) else {
        return 0;
    };
    let key = detail & 0xffff;
    if key != 0x7a && !(key == 0x1b && desktop.full()) {
        return 0;
    }
    if detail >> 16 == 0 {
        desktop.post(WM_COMMAND, FULL, 0);
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn lummix_desktop_error(
    token: *const c_void,
    message: PCWSTR,
    length: usize,
) {
    if let Some(desktop) = desktop(token) {
        if !message.is_null() {
            *desktop.failure.borrow_mut() = Some(
                std::slice::from_raw_parts(message, length)
                    .iter()
                    .copied()
                    .chain([0])
                    .collect(),
            );
        }
        desktop.post(FAILURE, 1, 0);
    }
}

#[no_mangle]
pub extern "C" fn lummix_desktop_notify(hwnd: HWND, event: u32) {
    let message = match event {
        UPDATE => UPDATE_CHANGED,
        SPOTIFY => SPOTIFY_CHANGED,
        BEGIN_EXIT => WM_COMMAND,
        _ => return,
    };
    unsafe { PostMessageW(hwnd, message, if event == BEGIN_EXIT { EXIT } else { 0 }, 0) };
}

#[no_mangle]
pub unsafe extern "C" fn lummix_desktop_startup_error(message: PCWSTR) {
    MessageBoxW(null_mut(), message, CAPTION, MB_ICONERROR);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::sync::Mutex;

    static EVENTS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
    static HWND_TEST: Mutex<()> = Mutex::new(());

    extern "C" fn callback(event: u32, data: *const u8, len: usize) -> i32 {
        if event == REMEMBER && len == size_of::<SavedPlacement>() && !data.is_null() {
            let saved = unsafe { &*data.cast::<SavedPlacement>() };
            if saved.geometry[2] <= 0.0 || saved.geometry[3] <= 0.0 || saved.screen[0] == 0 {
                return 0;
            }
        }
        EVENTS.lock().unwrap().push(event);
        0
    }

    #[test]
    fn actual_hwnd_keeps_identity_and_dispatches_close_in_the_rust_pump() {
        let _exclusive = HWND_TEST.lock().unwrap();
        let package = std::env::var_os("LUMMIX_TEST_PROJECT")
            .map(PathBuf::from)
            .map_or_else(
                || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../packaging"),
                |path| path.join("windows/packaging"),
            );
        let icon: Vec<u16> = package
            .join("Lummix.ico")
            .as_os_str()
            .encode_wide()
            .chain([0])
            .collect();
        unsafe {
            let token = lummix_desktop_create(icon.as_ptr(), null(), null(), Some(callback));
            assert!(!token.is_null());
            let hwnd = lummix_desktop_hwnd(token);
            assert!(!hwnd.is_null());
            assert_eq!(IsWindowVisible(hwnd), 0);
            let mut class = [0u16; 60];
            let class_len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) as usize;
            assert_eq!(
                String::from_utf16_lossy(&class[..class_len]),
                "LumMixStudioHost"
            );
            let mut caption = [0u16; 60];
            let caption_len =
                GetWindowTextW(hwnd, caption.as_mut_ptr(), caption.len() as i32) as usize;
            assert_eq!(String::from_utf16_lossy(&caption[..caption_len]), "Lummix");
            assert_ne!(SendMessageW(hwnd, WM_GETICON, 0, 0), 0);
            assert_eq!(SendMessageW(hwnd, CAN_REOPEN, 0, 0), 1);
            lummix_desktop_notify(hwnd, UPDATE);
            lummix_desktop_notify(hwnd, SPOTIFY);
            let close = wide("close");
            assert_eq!(
                lummix_desktop_command(token, close.as_ptr(), close.len() - 1),
                1
            );
            assert_eq!(lummix_desktop_pump(token), 0);
            assert!(lummix_desktop_hwnd(token).is_null());
            lummix_desktop_close(token);
        }
        assert_eq!(
            *EVENTS.lock().unwrap(),
            [
                UPDATE,
                SPOTIFY,
                CLOSE_TO_TRAY,
                BEGIN_EXIT,
                REMEMBER,
                FINISH_EXIT
            ]
        );
    }

    #[test]
    fn logical_window_placement_preserves_dpi_and_negative_work_areas() {
        let left = RECT {
            left: -1920,
            top: -50,
            right: 0,
            bottom: 1030,
        };
        assert_eq!(placement(&[], left, 96.0), [-1600, 75, 1280, 830]);
        assert_eq!(
            placement(&[-5000.0, 5000.0, 100.0, 900.0], left, 144.0),
            [-1920, -50, 330, 1080]
        );
        assert_eq!(
            placement(&[100.0, 100.0, 800.0, 400.0], left, 144.0),
            [-1770, 100, 1200, 600]
        );
    }

    #[derive(Default)]
    struct BrowserFixture {
        desktop: usize,
        browser: usize,
        round_trip: bool,
        released: bool,
        closed: bool,
    }

    static LIVE: Mutex<BrowserFixture> = Mutex::new(BrowserFixture {
        desktop: 0,
        browser: 0,
        round_trip: false,
        released: false,
        closed: false,
    });

    extern "C" fn live_desktop(event: u32, _: *const u8, _: usize) -> i32 {
        if event == FINISH_EXIT {
            let browser = {
                let mut state = LIVE.lock().unwrap();
                state.closed = true;
                std::mem::take(&mut state.browser) as *mut c_void
            };
            unsafe { webview::lummix_webview_close(browser) };
        }
        0
    }

    unsafe fn c_string(text: PCWSTR) -> String {
        if text.is_null() {
            return String::new();
        }
        let length = (0..).take_while(|&i| *text.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    }

    extern "C" fn live_browser(event: u32, _: PCWSTR, data: PCWSTR, _: u32) -> i32 {
        const WEB_MESSAGE: u32 = 2;
        if event == WEB_MESSAGE {
            let message = unsafe { c_string(data) };
            let desktop = LIVE.lock().unwrap().desktop as *const c_void;
            let command = match message.as_str() {
                "window-state-ok" => {
                    LIVE.lock().unwrap().round_trip = true;
                    "close"
                }
                "fixture-released" => {
                    LIVE.lock().unwrap().released = true;
                    return 0;
                }
                command => command,
            };
            let wide = wide(command);
            return unsafe { lummix_desktop_command(desktop, wide.as_ptr(), wide.len() - 1) };
        }
        0
    }

    #[test]
    #[ignore = "requires the signed Microsoft loader and installed Evergreen runtime"]
    fn desktop_pumps_actual_webview_window_state_and_release_handshake() {
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        let _exclusive = HWND_TEST.lock().unwrap();
        let root = std::env::var_os("LUMMIX_TEST_PROJECT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .canonicalize()
            .unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let fixture =
            std::env::temp_dir().join(format!("LummixDesktopTest-{}-{unique}", std::process::id()));
        let assets = fixture.join("studio");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(assets.join("index.html"), r#"
<!doctype html><meta charset="utf-8"><link rel="icon" href="data:,"><iframe id="canvas"></iframe><script>
document.querySelector('iframe').contentWindow.releaseLummixAudio = () => chrome.webview.postMessage('fixture-released');
chrome.webview.addEventListener('message', ({data}) => {
    if (data.type === 'lummix-window-state' && data.maximized === false && data.fullscreen === false) {
        chrome.webview.postMessage('window-state-ok');
    }
});
chrome.webview.postMessage('window-state');
</script>"#).unwrap();
        let path = |path: PathBuf| {
            path.as_os_str()
                .encode_wide()
                .chain([0])
                .collect::<Vec<u16>>()
        };
        let data = root.join(".build/frozen/runtime/_internal");
        let icon = path(data.join("Lummix.ico").canonicalize().unwrap());
        let loader = path(data.join("WebView2Loader.dll").canonicalize().unwrap());
        let profile = path(fixture.join("profile"));
        let assets = path(assets);
        unsafe {
            let token = lummix_desktop_create(icon.as_ptr(), null(), null(), Some(live_desktop));
            assert!(!token.is_null());
            LIVE.lock().unwrap().desktop = token as usize;
            let hwnd = lummix_desktop_hwnd(token);
            let mut browser = null_mut();
            assert_eq!(
                webview::lummix_webview_start(
                    windows::Win32::Foundation::HWND(hwnd),
                    loader.as_ptr(),
                    profile.as_ptr(),
                    assets.as_ptr(),
                    Some(live_browser),
                    &mut browser
                ),
                0
            );
            LIVE.lock().unwrap().browser = browser as usize;
            lummix_desktop_attach_web(token, browser);
            let (cancel, cancelled) = std::sync::mpsc::channel::<()>();
            let window = hwnd as usize;
            let watchdog = std::thread::spawn(move || {
                if cancelled.recv_timeout(Duration::from_secs(15)).is_err() {
                    PostMessageW(window as HWND, WM_CLOSE, 0, 0);
                }
            });
            assert_eq!(lummix_desktop_pump(token), 0);
            let _ = cancel.send(());
            watchdog.join().unwrap();
            lummix_desktop_close(token);
        }
        let state = LIVE.lock().unwrap();
        assert!(
            state.round_trip && state.released && state.closed,
            "native window state, WebView release callback, and closed HWND are all required"
        );
        let _ = std::fs::remove_dir_all(&fixture);
    }
}
