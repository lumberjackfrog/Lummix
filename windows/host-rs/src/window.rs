use std::mem::{size_of, zeroed};
use std::ptr::null;

use windows_sys::core::w;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromRect, MonitorFromWindow, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, GetWindowRect, IsZoomed, PostMessageW,
    SetForegroundWindow, TrackPopupMenu, HICON, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCLIENT,
    HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, MF_SEPARATOR, MF_STRING, MINMAXINFO,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_COMMAND, WM_GETMINMAXINFO, WM_NCCALCSIZE, WM_NCHITTEST,
};

fn tray_data(hwnd: HWND, icon: HICON, callback: u32) -> NOTIFYICONDATAW {
    // The flags and size describe a full, zero-initialized NOTIFYICONDATAW.
    let mut data: NOTIFYICONDATAW = unsafe { zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = 1;
    data.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE | NIF_SHOWTIP;
    data.uCallbackMessage = callback;
    data.hIcon = icon;
    for (target, unit) in data.szTip.iter_mut().zip("Lummix".encode_utf16()) {
        *target = unit;
    }
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    data
}

#[no_mangle]
pub extern "C" fn lummix_tray_show(hwnd: HWND, icon: HICON, callback: u32, registered: i32) -> i32 {
    let data = tray_data(hwnd, icon, callback);
    // The shell may have restarted and lost the previously registered icon.
    unsafe {
        if registered != 0 && Shell_NotifyIconW(NIM_MODIFY, &data) != 0 {
            return 1;
        }
        if Shell_NotifyIconW(NIM_ADD, &data) == 0 {
            return 0;
        }
        Shell_NotifyIconW(NIM_SETVERSION, &data);
    }
    1
}

#[no_mangle]
pub extern "C" fn lummix_tray_hide(hwnd: HWND, icon: HICON, callback: u32) {
    let data = tray_data(hwnd, icon, callback);
    unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
}

#[no_mangle]
pub extern "C" fn lummix_tray_menu(
    hwnd: HWND,
    open: usize,
    full: usize,
    exit: usize,
    x: i32,
    y: i32,
) {
    // The menu and its popup loop are owned by the originating UI thread.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        AppendMenuW(menu, MF_STRING, open, w!("Open Lummix"));
        AppendMenuW(menu, MF_STRING, full, w!("Full screen"));
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, exit, w!("Exit Lummix"));
        SetForegroundWindow(hwnd);
        let mut position = POINT { x, y };
        GetCursorPos(&mut position);
        let selected = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            position.x,
            position.y,
            0,
            hwnd,
            null(),
        );
        if selected != 0 {
            PostMessageW(hwnd, WM_COMMAND, selected as usize, 0);
        }
        DestroyMenu(menu);
    }
}

fn dpi(hwnd: HWND) -> u32 {
    match unsafe { GetDpiForWindow(hwnd) } {
        0 => 96,
        value => value,
    }
}

fn scaled(value: u32, dpi: u32) -> i32 {
    (f64::from(value) * f64::from(dpi) / 96.0).round_ties_even() as i32
}

#[no_mangle]
pub extern "C" fn lummix_frame_edge(hwnd: HWND, fullscreen: i32) -> i32 {
    if fullscreen != 0 || unsafe { IsZoomed(hwnd) } != 0 {
        return 0;
    }
    scaled(5, dpi(hwnd)).max(3)
}

fn resize_hit(rect: &RECT, x: i32, y: i32, edge: i32) -> i32 {
    fn axis(value: i32, low: i32, high: i32, edge: i32) -> usize {
        if value < low + edge {
            0
        } else if value >= high - edge {
            2
        } else {
            1
        }
    }
    [
        [HTTOPLEFT, HTTOP, HTTOPRIGHT],
        [HTLEFT, HTCLIENT, HTRIGHT],
        [HTBOTTOMLEFT, HTBOTTOM, HTBOTTOMRIGHT],
    ][axis(y, rect.top, rect.bottom, edge)][axis(x, rect.left, rect.right, edge)] as i32
}

fn monitor_info(monitor: HMONITOR) -> Option<MONITORINFO> {
    let mut info: MONITORINFO = unsafe { zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    (unsafe { GetMonitorInfoW(monitor, &mut info) } != 0).then_some(info)
}

/// The lparam must be supplied by the UI thread for the given Windows message.
#[no_mangle]
pub unsafe extern "C" fn lummix_frame_message(
    hwnd: HWND,
    message: u32,
    lparam: LPARAM,
    fullscreen: i32,
) -> isize {
    match message {
        WM_NCCALCSIZE if fullscreen == 0 && lparam != 0 && IsZoomed(hwnd) != 0 => {
            // For either wParam form, a RECT is the first member at lParam.
            let bounds = &mut *(lparam as *mut RECT);
            let monitor = MonitorFromRect(bounds, MONITOR_DEFAULTTONEAREST);
            if let Some(info) = monitor_info(monitor) {
                bounds.left = bounds.left.max(info.rcWork.left);
                bounds.top = bounds.top.max(info.rcWork.top);
                bounds.right = bounds.right.min(info.rcWork.right);
                bounds.bottom = bounds.bottom.min(info.rcWork.bottom);
            }
        }
        WM_NCHITTEST => {
            let edge = lummix_frame_edge(hwnd, fullscreen);
            let mut rect: RECT = zeroed();
            if edge != 0 && GetWindowRect(hwnd, &mut rect) != 0 {
                // LPARAM packs signed coordinates, including monitors left of the primary.
                let x = lparam as i16 as i32;
                let y = (lparam >> 16) as i16 as i32;
                return resize_hit(&rect, x, y, edge) as isize;
            }
            return HTCLIENT as isize;
        }
        WM_GETMINMAXINFO if lparam != 0 => {
            let bounds = &mut *(lparam as *mut MINMAXINFO);
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            if let Some(info) = monitor_info(monitor) {
                let area = if fullscreen != 0 {
                    info.rcMonitor
                } else {
                    info.rcWork
                };
                bounds.ptMaxPosition = POINT {
                    x: area.left - info.rcMonitor.left,
                    y: area.top - info.rcMonitor.top,
                };
                bounds.ptMaxSize = POINT {
                    x: area.right - area.left,
                    y: area.bottom - area.top,
                };
            }
            let dpi = dpi(hwnd);
            bounds.ptMinTrackSize = POINT {
                x: scaled(320, dpi),
                y: scaled(360, dpi),
            };
        }
        _ => {}
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null_mut;

    #[test]
    fn hits_signed_multi_monitor_coordinates_and_boundaries() {
        let area = RECT {
            left: -800,
            top: -500,
            right: -100,
            bottom: 150,
        };
        assert_eq!(resize_hit(&area, -798, -497, 5), HTTOPLEFT as i32);
        assert_eq!(resize_hit(&area, -105, -495, 5), HTRIGHT as i32);
        assert_eq!(resize_hit(&area, -795, -495, 5), HTCLIENT as i32);
        assert_eq!(resize_hit(&area, -770, 149, 5), HTBOTTOM as i32);
        assert_eq!(scaled(5, 120), 6);
        assert_eq!(scaled(5, 144), 8);
    }

    #[test]
    fn shell_notification_keeps_identity_and_v4_protocol() {
        let data = tray_data(null_mut(), null_mut(), 0x8122);
        assert_eq!(data.cbSize as usize, size_of::<NOTIFYICONDATAW>());
        assert_eq!(data.uID, 1);
        assert_eq!(data.uCallbackMessage, 0x8122);
        assert_eq!(unsafe { data.Anonymous.uVersion }, 4);
        assert_eq!(&data.szTip[..7], &[76, 117, 109, 109, 105, 120, 0]);
    }
}
