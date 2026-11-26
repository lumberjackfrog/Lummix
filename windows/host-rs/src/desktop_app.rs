use std::cell::Cell;
use std::env;
use std::ffi::{c_void, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::Arc;

use serde_json::{json, Map, Value};
use windows::core::PCWSTR;
use windows_sys::core::w;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};

use crate::{
    api_runtime, desktop, desktop_jobs::Jobs, diagnostics, lifecycle, profile, release_feed,
    setup_runtime, sign_in, studio, webview,
};

const GEOMETRY: [&str; 4] = ["x", "y", "width", "height"];

struct App {
    executable: PathBuf,
    profile: PathBuf,
    version: &'static str,
    jobs: Arc<Jobs>,
    desktop: Cell<*mut c_void>,
    browser: Cell<*mut c_void>,
    window: Cell<HWND>,
}

thread_local! {
    static ACTIVE: Cell<*const App> = const { Cell::new(null()) };
}

struct Registered;

impl Registered {
    fn new(app: &App) -> Result<Self, String> {
        ACTIVE.with(|active| {
            if !active.get().is_null() {
                return Err("Lummix already owns a window on this thread.".into());
            }
            active.set(app);
            Ok(Self)
        })
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(null()));
    }
}

fn app(operation: impl FnOnce(&App) -> i32) -> i32 {
    ACTIVE.with(|active| {
        // The window and all WebView callbacks run on this thread. The app outlives
        // both handles, which are closed before clearing this registration.
        match unsafe { active.get().as_ref() } {
            Some(app) => operation(app),
            None => 0,
        }
    })
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn text_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

unsafe fn callback_text(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    PCWSTR(text).to_string().unwrap_or_default()
}

fn open_item(address: &[u16]) -> bool {
    let status = unsafe {
        ShellExecuteW(
            null_mut(),
            w!("open"),
            address.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    status as isize > 32
}

fn external_allowed(address: &str) -> bool {
    if address == "spotify:" {
        return true;
    }
    let Some(address) = address.strip_prefix("https://") else {
        return false;
    };
    if address
        .bytes()
        .any(|byte| matches!(byte, 0..=32 | 127 | b'\\'))
    {
        return false;
    }
    let authority = address.split(['/', '?', '#']).next().unwrap_or_default();
    [
        "vb-audio.com",
        "www.vb-audio.com",
        "www.spotify.com",
        "open.spotify.com",
    ]
    .iter()
    .any(|host| authority.eq_ignore_ascii_case(host))
}

fn valid_placement(data: &Map<String, Value>) -> bool {
    GEOMETRY.iter().all(|key| {
        data.get(*key)
            .and_then(Value::as_f64)
            .is_some_and(|value| value.abs() < 20_000.0)
    })
}

fn saved_placement(profile: &Path) -> ([f64; 4], Option<String>, bool) {
    let state = profile::load_profile(&profile.join("window-placement.json"), valid_placement)
        .unwrap_or_default();
    let values = GEOMETRY.map(|key| state.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN));
    let screen = state
        .get("screen")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let maximized = state.get("maximized").and_then(Value::as_bool) == Some(true);
    (values, screen, maximized)
}

fn remember(profile: &Path, placement: &desktop::SavedPlacement) {
    let Some(values) = GEOMETRY
        .iter()
        .zip(placement.geometry)
        .map(|(key, value)| {
            serde_json::Number::from_f64(value)
                .map(|value| ((*key).to_owned(), Value::Number(value)))
        })
        .collect::<Option<Map<_, _>>>()
    else {
        return;
    };
    let mut data = values;
    let length = placement
        .screen
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(placement.screen.len());
    data.insert(
        "screen".to_owned(),
        String::from_utf16_lossy(&placement.screen[..length]).into(),
    );
    data.insert("maximized".to_owned(), (placement.maximized != 0).into());
    let _ = profile::save_profile(&profile.join("window-placement.json"), &data);
}

fn close_to_tray(profile: &Path) -> bool {
    let state = profile::load_profile(&profile.join("studio-preferences.json"), |data| {
        data.get("settings")
            .and_then(Value::as_object)
            .is_some_and(profile::valid_studio_settings)
    })
    .unwrap_or_default();
    state
        .get("settings")
        .and_then(|state| state["closeToTray"].as_bool())
        != Some(false)
}

impl App {
    fn post(&self, data: Value) {
        let text = text_wide(&data.to_string());
        unsafe {
            webview::lummix_webview_post_json(self.browser.get(), text.as_ptr());
        }
    }
    fn source(&self) -> Option<String> {
        unsafe { webview::document_source(self.browser.get()) }
    }
    fn log(&self, event: &str) {
        diagnostics::record(&self.profile, self.version, event);
    }
    fn diagnostics(&self, open: bool) {
        let result = if open {
            let folder = self.profile.join("Diagnostics");
            std::fs::create_dir_all(&folder).is_ok() && {
                diagnostics::snapshot(&self.profile, self.version);
                open_item(&wide(&folder))
            }
        } else {
            diagnostics::delete_logs(&self.profile).is_ok()
        };
        let message = match (result, open) {
            (true, true) => "Opened the local diagnostics folder.",
            (true, false) => "Local logs cleared.",
            _ => "Windows couldn’t access the diagnostics folder.",
        };
        self.post(json!({"type":"lummix-diagnostics","message":message}));
    }

    fn command(&self, source: &str, message: &str) -> i32 {
        if !source.starts_with(webview::ORIGIN) {
            return 0;
        }
        let command = text_wide(message);
        let handled = unsafe {
            desktop::lummix_desktop_command(self.desktop.get(), command.as_ptr(), command.len() - 1)
        };
        if handled != 0 || source != webview::APP_DOCUMENT {
            return handled;
        }
        match message {
            "spotify-setup" | "spotify-setup-state" if !self.jobs.stopping() => {
                if message == "spotify-setup" {
                    self.jobs.spotify_begin();
                }
                self.post(self.jobs.spotify_state());
            }
            "update-state" | "update-check" | "update-download" | "update-install"
                if !self.jobs.stopping() =>
            {
                if message != "update-state" {
                    self.jobs
                        .update_command(message.trim_start_matches("update-"));
                }
                self.post(self.jobs.update_state());
            }
            "logs-open" | "logs-clear" => self.diagnostics(message == "logs-open"),
            "startup-state" => self.post(sign_in::response(&self.executable, None)),
            "startup-on" | "startup-off" => {
                self.post(sign_in::response(
                    &self.executable,
                    Some(message == "startup-on"),
                ));
            }
            _ => return 0,
        }
        1
    }
}

extern "C" fn desktop_event(event: u32, data: *const u8, length: usize) -> i32 {
    app(|app| {
        match event {
            desktop::CLOSE_TO_TRAY => return close_to_tray(&app.profile).into(),
            desktop::REMEMBER
                if !data.is_null() && length == size_of::<desktop::SavedPlacement>() =>
            {
                // The desktop supplies a borrowed, aligned placement for the duration of this callback.
                remember(&app.profile, unsafe {
                    &*data.cast::<desktop::SavedPlacement>()
                });
            }
            desktop::BEGIN_EXIT => app.jobs.stop(),
            desktop::FINISH_EXIT => {
                app.jobs.stop();
                app.window.set(null_mut());
                let view = app.browser.replace(null_mut());
                unsafe { webview::lummix_webview_close(view) };
            }
            desktop::UPDATE => {
                let state = app.jobs.update_state();
                if state["phase"] == "launched" {
                    desktop::lummix_desktop_notify(app.window.get(), desktop::BEGIN_EXIT);
                } else if app.source().as_deref() == Some(webview::APP_DOCUMENT) {
                    app.post(state);
                }
            }
            desktop::SPOTIFY if app.source().as_deref() == Some(webview::APP_DOCUMENT) => {
                app.post(app.jobs.spotify_state());
            }
            desktop::WINDOW_FAILED => app.log("window_failed"),
            _ => {}
        }
        0
    })
}

extern "C" fn web_event(event: u32, source: *const u16, value: *const u16, detail: u32) -> i32 {
    app(|app| {
        match event {
            webview::READY => app.jobs.update_command("auto"),
            webview::ERROR => {
                let error = unsafe { callback_text(value) };
                let error = text_wide(&error);
                unsafe {
                    desktop::lummix_desktop_error(
                        app.desktop.get(),
                        error.as_ptr(),
                        error.len() - 1,
                    );
                }
            }
            webview::MESSAGE => {
                return app.command(&unsafe { callback_text(source) }, &unsafe {
                    callback_text(value)
                });
            }
            webview::NEW_PAGE => {
                let address = unsafe { callback_text(value) };
                if external_allowed(&address) {
                    open_item(&text_wide(&address));
                }
            }
            webview::KEY => {
                return unsafe { desktop::lummix_desktop_accelerator(app.desktop.get(), detail) }
            }
            _ => {}
        }
        0
    })
}

fn startup_failure(status: u32, root: &Path, version: &'static str) -> String {
    let port = [41835, 41836, 41838];
    let service = ["studio", "wrist", "spotify"];
    let errors = ["studio_failed", "wrist_failed", "spotify_failed"];
    let index = (status % 10) as usize;
    if matches!(status, 10..=12 | 20..=22 | 30..=32) {
        diagnostics::record(root, version, errors[index]);
    }
    match status {
        3 | 22 => "Port 41838 is already in use by an incompatible service. Close any earlier Lummix window or stop its media helper, then reopen this version of Lummix.".into(),
        10..=12 => "Extract the complete Lummix ZIP into one folder and open Lummix again.".into(),
        21 => "Port 41836 is in use. Close any older Lummix wrist preview or setup helper and reopen Lummix.".into(),
        20 => format!("Lummix could not identify the service on port {}. Close the conflicting app or try again after startup.", port[index]),
        30..=32 => format!("The {} service did not start on port {}. Extract the complete download and try again.", service[index], port[index]),
        _ => "Windows could not start the Lummix host. Reopen Lummix and try again.".into(),
    }
}

fn launch(executable: &Path, data: &Path) -> Result<(), String> {
    let user =
        env::var_os("LOCALAPPDATA").ok_or("Windows did not provide a writable user profile.")?;
    let profile = PathBuf::from(user).join("LumMix");
    let version =
        release_feed::identity("VERSION").map_err(|_| "Lummix has invalid version metadata.")?;
    let assets = executable.with_file_name("studio");
    let build = diagnostics::windows_build();
    lifecycle::lummix_host_dpi();
    let result = (|| {
        let _runtime =
            match lifecycle::prepare(executable.to_owned(), assets.clone(), build >= 20348) {
                Ok(runtime) => runtime,
                Err(1) => return Ok(()),
                Err(status) => return Err(startup_failure(status, &profile, version)),
            };
        let _addon = setup_runtime::start_addon(data).ok().flatten();
        let app = App {
            executable: executable.to_owned(),
            profile: profile.clone(),
            version,
            jobs: Jobs::new(&profile, version),
            desktop: Cell::new(null_mut()),
            browser: Cell::new(null_mut()),
            window: Cell::new(null_mut()),
        };
        let _registered = Registered::new(&app)?;
        let result = app.run(data, &assets, build);
        app.jobs.stop();
        let window = app.desktop.replace(null_mut());
        unsafe { desktop::lummix_desktop_close(window) };
        let view = app.browser.replace(null_mut());
        unsafe { webview::lummix_webview_close(view) };
        result
    })();
    if let Err(error) = &result {
        diagnostics::record(&profile, version, "startup_failed");
        unsafe { desktop::lummix_desktop_startup_error(text_wide(error).as_ptr()) };
    }
    result
}

impl App {
    fn run(&self, data: &Path, assets: &Path, build: u32) -> Result<(), String> {
        let (saved, screen, maximized) = saved_placement(&self.profile);
        let screen = screen.as_deref().map(text_wide);
        let screen = screen.as_ref().map_or(null(), Vec::as_ptr);
        let icon = wide(&data.join("Lummix.ico"));
        let desktop = unsafe {
            desktop::lummix_desktop_create(
                icon.as_ptr(),
                screen,
                saved.as_ptr(),
                Some(desktop_event),
            )
        };
        if desktop.is_null() {
            return Err("Windows could not create the Lummix studio window.".into());
        }
        self.desktop.set(desktop);
        let hwnd = unsafe { desktop::lummix_desktop_hwnd(desktop) };
        self.window.set(hwnd);
        self.jobs.attach(hwnd as usize);
        self.log("opened");
        unsafe { desktop::lummix_desktop_show(desktop, maximized.into(), (build >= 22000).into()) };
        let loader = wide(&data.join("WebView2Loader.dll"));
        let profile = wide(&self.profile.join("Studio WebView"));
        let assets = wide(assets);
        let mut view = null_mut();
        let status = unsafe {
            webview::lummix_webview_start(
                windows::Win32::Foundation::HWND(hwnd),
                loader.as_ptr(),
                profile.as_ptr(),
                assets.as_ptr(),
                Some(web_event),
                &mut view,
            )
        };
        if status < 0 {
            return Err("Microsoft Edge WebView2 Runtime could not start. Install the Evergreen x64 runtime from Microsoft and reopen Lummix.".into());
        }
        self.browser.set(view);
        unsafe { desktop::lummix_desktop_attach_web(desktop, view) };
        if unsafe { desktop::lummix_desktop_pump(desktop) } < 0 {
            Err("Windows could not dispatch the Lummix studio messages.".into())
        } else {
            self.log("closed");
            Ok(())
        }
    }
}

fn setup_result(result: io::Result<()>) -> Result<(), String> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn standalone_studio(executable: &Path, args: &[OsString]) -> Result<(), String> {
    let linked = match args {
        [] => false,
        [argument] if argument == "--parent-stdin" => true,
        _ => return Err("Usage: Lummix.exe studio [--parent-stdin]".into()),
    };
    let bind = if env::var("LUMMIX_QA_LOOPBACK").as_deref() == Ok("2") {
        "127.0.0.2:41835"
    } else {
        studio::HOST
    };
    let server = tiny_http::Server::http(bind).map_err(|error| error.to_string())?;
    if linked {
        std::thread::Builder::new()
            .name("Lummix parent stdin".into())
            .spawn(|| {
                let _ = io::copy(&mut io::stdin().lock(), &mut io::sink());
                std::process::exit(0);
            })
            .map_err(|error| error.to_string())?;
    }
    studio::serve(server, &executable.with_file_name("studio")).map_err(|error| error.to_string())
}

pub(crate) fn entry(executable: &Path, args: &[OsString]) -> Result<(), String> {
    let data = executable.with_file_name("_internal");
    let (mode, args) = match args.split_first() {
        None => ("launch", &[][..]),
        Some((mode, args)) => (mode.to_str().ok_or("Invalid Lummix command.")?, args),
    };
    match (mode, args) {
        ("launch", []) => return launch(executable, &data),
        ("--resume-setup", []) => {
            if setup_runtime::resume_login(executable, &data) {
                launch(executable, &data)?;
            }
            return Ok(());
        }
        ("--startup", []) => {
            if sign_in::enabled(executable).unwrap_or(false) {
                launch(executable, &data)?;
            }
            return Ok(());
        }
        ("refresh-startup", []) => {
            setup_result(sign_in::refresh_resume(executable))?;
            return setup_result(sign_in::refresh(executable));
        }
        ("remove-addon", []) => {
            setup_runtime::clear_resume(executable);
            setup_result(sign_in::change(executable, false))?;
        }
        ("studio", _) => return standalone_studio(executable, args),
        ("verify-services", [flag])
            if flag == "--parent-stdin" && env::var("LUMMIX_QA_LOOPBACK").as_deref() == Ok("2") =>
        {
            let _owner = lifecycle::prepare_on(
                executable.to_owned(),
                executable.with_file_name("studio"),
                diagnostics::windows_build() >= 20348,
                true,
            )
            .map_err(|status| format!("Lummix service verification failed ({status})."))?;
            use io::Write;
            let mut output = io::stdout().lock();
            writeln!(output, r#"{{"service":"lummix-lifecycle","ready":true}}"#)
                .and_then(|()| output.flush())
                .map_err(|error| error.to_string())?;
            return io::copy(&mut io::stdin().lock(), &mut io::sink())
                .map(|_| ())
                .map_err(|error| error.to_string());
        }
        ("diagnostics", _) => return diagnostics::run(args).map_err(|error| error.to_string()),
        ("sign-in", _) => {
            if let Err(error) = sign_in::run(args) {
                if error.kind() == io::ErrorKind::PermissionDenied {
                    std::process::exit(5);
                }
                return Err(error.to_string());
            }
            return Ok(());
        }
        _ => {}
    }
    if setup_runtime::run_mode(mode, args, &data)? || api_runtime::run_mode(mode, args, &data)? {
        return Ok(());
    }
    Err(format!("Unsupported Lummix command: {mode}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_popup_policy_matches_only_expected_product_destinations() {
        for allowed in [
            "spotify:",
            "https://www.spotify.com/download/windows/",
            "https://vb-audio.com/Cable/",
            "https://open.spotify.com/track/example",
        ] {
            assert!(external_allowed(allowed));
        }
        for denied in [
            "file:///C:/ProgramData/setup.exe",
            "http://www.spotify.com/",
            "https://www.spotify.com.example.org/",
            "https://www.spotify.com@other.example/",
            "https://www.spotify.com\\@other.example/",
        ] {
            assert!(!external_allowed(denied));
        }
    }
}
