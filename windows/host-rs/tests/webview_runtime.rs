#![cfg(windows)]

#[path = "../src/studio_files.rs"]
mod studio_files;
#[path = "../src/webview.rs"]
mod webview;

use std::env;
use std::ffi::{c_void, OsStr};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<link rel="icon" href="data:,">
<title>Lummix native controller fixture</title>
<script>
window.chrome.webview.addEventListener('message', async (event) => {
  const response = await fetch('controls.wasm?qa=real');
  const payload = new Uint8Array(await response.arrayBuffer());
  chrome.webview.postMessage(JSON.stringify({
    echo: event.data.fixture, mime: response.headers.get('Content-Type'),
    cache: response.headers.get('Cache-Control'),
    wasm: payload.slice(0, 4).join(','), size: payload.length,
  }));
});
chrome.webview.postMessage('fixture-requests-host');
</script>"#;

static EVENTS: Mutex<Vec<(u32, String, String)>> = Mutex::new(Vec::new());

extern "C" fn events(kind: u32, source: *const u16, data: *const u16, _: u32) -> i32 {
    let decode = |text: *const u16| {
        if text.is_null() {
            String::new()
        } else {
            unsafe { PCWSTR(text).to_string() }.unwrap_or_default()
        }
    };
    let source = decode(source);
    let data = decode(data);
    EVENTS.lock().unwrap().push((kind, source, data));
    0
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn window() -> windows::core::Result<HWND> {
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None)?.0);
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            lpszClassName: w!("LummixRustControllerAbiFixture"),
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(
            Default::default(),
            class.lpszClassName,
            w!("Lummix Rust Controller ABI Fixture"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            900,
            650,
            None,
            None,
            Some(instance),
            None,
        )
    }
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

fn fail(message: impl Into<String>) -> Box<dyn std::error::Error> {
    io::Error::other(message.into()).into()
}

fn command(status: i32, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if status < 0 {
        return Err(fail(format!("{name} failed with HRESULT {status:x}")));
    }
    Ok(())
}

fn pump(
    hwnd: HWND,
    token: *mut c_void,
    expected_size: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let until = Instant::now() + Duration::from_secs(15);
    let mut msg = MSG::default();
    let mut rendered = false;
    let mut source_check = None;
    unsafe {
        if SetTimer(Some(hwnd), 1, 50, None) == 0 {
            return Err(fail("Fixture deadline timer failed"));
        }
        while Instant::now() < until {
            let events = std::mem::take(&mut *EVENTS.lock().unwrap());
            for (kind, source, data) in events {
                if kind == 1 {
                    return Err(fail(data));
                }
                if kind == 0 {
                    command(webview::lummix_webview_resize(token, 800, 600), "resize")?;
                    command(webview::lummix_webview_moved(token), "move")?;
                    command(webview::lummix_webview_visible(token, true as i32), "show")?;
                }
                if kind == 2 && source == "http://127.0.0.1:41835/?view=desktop&app=1" {
                    if data == "fixture-requests-host" {
                        command(
                            webview::lummix_webview_post_json(
                                token,
                                w!(r#"{"fixture":"native"}"#).0,
                            ),
                            "post",
                        )?;
                    } else {
                        let expected = format!(
                            r#"{{"echo":"native","mime":"application/wasm","cache":"no-store","wasm":"0,97,115,109","size":{expected_size}}}"#
                        );
                        if data != expected {
                            return Err(fail(format!("Invalid actual WebView reply: {data}")));
                        }
                        command(
                            webview::lummix_webview_execute(
                                token,
                                w!("location.href='http://127.0.0.1:9/should-be-cancelled';").0,
                            ),
                            "execute",
                        )?;
                        source_check = Some(Instant::now() + Duration::from_millis(500));
                        rendered = true;
                    }
                }
            }
            if rendered && source_check.is_some_and(|at| Instant::now() >= at) {
                let len = webview::lummix_webview_source(token, null_mut(), 0);
                let mut wide = vec![0; len];
                webview::lummix_webview_source(token, wide.as_mut_ptr(), len);
                let source = String::from_utf16_lossy(wide.strip_suffix(&[0]).unwrap_or(&wide));
                if source != "http://127.0.0.1:41835/?view=desktop&app=1" {
                    return Err(fail(format!(
                        "Off-origin navigation changed native WebView source to {source}"
                    )));
                }
                return Ok(());
            }
            if GetMessageW(&mut msg, None, 0, 0).0 <= 0 {
                return Err(fail(
                    "Native WebView window closed before rendering fixture",
                ));
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Err(fail("Production Rust WebView ABI fixture timed out"))
}

fn run(loader: &Path, fixture: &Path, wasm: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let profile = fixture.join("foreign profile");
    let assets = fixture.join("installed Studio");
    std::fs::create_dir_all(&assets)?;
    std::fs::write(assets.join("index.html"), PAGE)?;
    let expected_size = std::fs::copy(wasm, assets.join("controls.wasm"))? as usize;
    let hwnd = window()?;
    let loader = wide(loader.as_os_str());
    let profile = wide(profile.as_os_str());
    let assets = wide(assets.as_os_str());
    let mut token = null_mut();
    unsafe {
        command(
            webview::lummix_webview_start(
                hwnd,
                loader.as_ptr(),
                profile.as_ptr(),
                assets.as_ptr(),
                Some(events),
                &mut token,
            ),
            "start",
        )?;
        let result = pump(hwnd, token, expected_size);
        webview::lummix_webview_close(token);
        let _ = DestroyWindow(hwnd);
        result?;
    }
    println!("Production Rust WebView ABI passed: actual Evergreen controller, installed-origin resource and real WASM, bounded native callbacks, bidirectional messaging, resize/show, and rejected off-origin navigation.");
    Ok(())
}

#[test]
#[ignore = "requires packaged Microsoft loader, Evergreen WebView2, and built Studio controls"]
fn actual_evergreen_controller_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let root = env::var_os("LUMMIX_TEST_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .canonicalize()?;
    let loader = root.join(".build/webview2/WebView2Loader.dll");
    let wasm = root.join("studio/controls.wasm");
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let fixture = env::temp_dir().join(format!(
        "Lummix-WebView2-Test-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir(&fixture)?;
    let result = run(&loader, &fixture, &wasm);
    let _ = std::fs::remove_dir_all(&fixture);
    result
}
