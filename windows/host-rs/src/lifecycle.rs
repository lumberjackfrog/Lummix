use std::ffi::c_void;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::ptr::{null, null_mut};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use windows_sys::core::{w, PCWSTR};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_ABANDONED,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, ReleaseMutex, WaitForSingleObject, CREATE_NO_WINDOW,
};
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW,
    SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_COMMAND,
};

use crate::local_http::LoopbackClient;

const OWNED: u32 = 0;
const ALREADY_RUNNING: u32 = 1;
const WINDOWS_ERROR: u32 = 2;
const RUNNING_MEDIA_CONFLICT: u32 = 3;
const MISSING_EXECUTABLE: u32 = 10;
const PORT_CONFLICT: u32 = 20;
const SERVICE_FAILED: u32 = 30;

#[derive(Clone, Copy)]
enum Service {
    Studio = 0,
    Wrist = 1,
    Media = 2,
}

impl Service {
    fn name(self) -> &'static str {
        match self {
            Self::Studio => "studio",
            Self::Wrist => "wrist",
            Self::Media => "spotify",
        }
    }

    fn port(self) -> u16 {
        match self {
            Self::Studio => 41835,
            Self::Wrist => 41836,
            Self::Media => 41838,
        }
    }

    fn matches(self, http: &LoopbackClient, modern_media: bool) -> bool {
        self.matches_at(http, self.port(), modern_media)
            .unwrap_or(false)
    }

    fn matches_at(self, http: &LoopbackClient, port: u16, modern_media: bool) -> io::Result<bool> {
        const CHECKED_BYTES: usize = 131_072;
        match self {
            Self::Studio => {
                let landing = http.get(port, "/", false, CHECKED_BYTES)?;
                let mixer = http.get(port, "/Mixer%20preview.html", false, CHECKED_BYTES)?;
                Ok(landing.status == 200
                    && mixer.status == 200
                    && contains(&landing.body, b"design-board.js")
                    && contains(&mixer.body, b"id=\"lum-visualizer\""))
            }
            service => {
                let reply =
                    http.get(port, "/lummix-health", matches!(service, Self::Media), 4097)?;
                if reply.status != 200 || reply.body.len() > 4096 {
                    return Ok(false);
                }
                let health: Value = serde_json::from_slice(&reply.body)?;
                if matches!(service, Self::Wrist) {
                    return Ok(health["service"] == "lummix-wrist"
                        && health["ready"] == true
                        && health["cableRestartSupport"] == true);
                }
                Ok(reply.studio_cors && compatible_media(&health, modern_media))
            }
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|part| part == needle)
}

fn compatible_media(health: &Value, modern: bool) -> bool {
    if health["service"] != "lummix-media" || health["protocol"] != 2 {
        return false;
    }
    let Some(supplied) = health["capabilities"].as_array() else {
        return false;
    };
    let supports = |required| supplied.iter().any(|entry| entry == required);
    [
        "spotify-session-peak-v1",
        "mic-route-v1",
        "mixer-duck-v1",
        "studio-preferences-v1",
        "mix-setup-alias-v1",
        "cable-restart-v1",
        "spotify-detection-v2",
        "spotify-window-artwork-v1",
    ]
    .into_iter()
    .all(supports)
        && (!modern || supports("spotify-process-mix-v1"))
}

struct Instance {
    handle: HANDLE,
    owned: bool,
}

impl Instance {
    fn acquire(name: PCWSTR) -> io::Result<Self> {
        // The initial owner is the calling thread only if this call creates the mutex.
        let (handle, already_exists) = unsafe {
            SetLastError(0);
            let handle = CreateMutexW(null(), 1, name);
            (handle, GetLastError() == ERROR_ALREADY_EXISTS)
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle,
            owned: !already_exists,
        })
    }

    fn wait(&mut self) {
        // An abandoned mutex also transfers ownership to this thread.
        self.owned = matches!(
            unsafe { WaitForSingleObject(self.handle, 180) },
            WAIT_OBJECT_0 | WAIT_ABANDONED
        );
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // Python opens and closes the lifecycle token on the originating UI thread.
        unsafe {
            if self.owned {
                ReleaseMutex(self.handle);
            }
            CloseHandle(self.handle);
        }
    }
}

struct Runtime {
    children: Vec<Child>,
    instance: Instance,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        for child in &mut self.children {
            drop(child.stdin.take());
        }
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline {
            self.children
                .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
            if self.children.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        // Release the window's instance mutex after signaling the owned helpers.
    }
}

impl Runtime {
    fn ensure(
        &mut self,
        service: Service,
        helper: &PathBuf,
        http: &LoopbackClient,
        modern: bool,
    ) -> Result<(), u32> {
        let port = service.port();
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        if TcpStream::connect_timeout(&address, Duration::from_millis(400)).is_ok() {
            return if service.matches(http, modern) {
                Ok(())
            } else {
                Err(PORT_CONFLICT + service as u32)
            };
        }
        if !helper.is_file() {
            return Err(MISSING_EXECUTABLE + service as u32);
        }
        let child = Command::new(helper)
            .args([service.name(), "--parent-stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|_| SERVICE_FAILED + service as u32)?;
        self.children.push(child);
        let child = self.children.last_mut().unwrap();
        let deadline = Instant::now() + Duration::from_secs(22);
        while Instant::now() < deadline {
            if service.matches(http, modern) {
                return Ok(());
            }
            if !matches!(child.try_wait(), Ok(None)) {
                break;
            }
            thread::sleep(Duration::from_millis(180));
        }
        Err(SERVICE_FAILED + service as u32)
    }
}

struct WindowProtocol {
    class: PCWSTR,
    can_reopen: u32,
    open: usize,
}

fn prepare(helper: PathBuf, protocol: WindowProtocol, modern: bool) -> Result<Runtime, u32> {
    let mut runtime = Runtime {
        children: Vec::new(),
        instance: Instance::acquire(w!("Local\\LummixNativeStudio")).map_err(|_| WINDOWS_ERROR)?,
    };
    let http = LoopbackClient::new().map_err(|_| WINDOWS_ERROR)?;
    if !runtime.instance.owned {
        for _ in 0..60 {
            let window = unsafe { FindWindowW(protocol.class, null()) };
            let mut can_reopen = 0;
            // Query the existing window, bounding how long an unresponsive HWND may block us.
            let answered = !window.is_null()
                && unsafe {
                    SendMessageTimeoutW(
                        window,
                        protocol.can_reopen,
                        0,
                        0,
                        SMTO_ABORTIFHUNG,
                        300,
                        &mut can_reopen,
                    )
                } != 0;
            if answered && can_reopen != 0 {
                if !Service::Media.matches(&http, modern) {
                    return Err(RUNNING_MEDIA_CONFLICT);
                }
                let mut pid = 0;
                // The already-running process will handle the same WM_COMMAND as its tray menu.
                unsafe {
                    GetWindowThreadProcessId(window, &mut pid);
                    AllowSetForegroundWindow(pid);
                    PostMessageW(window, WM_COMMAND, protocol.open, 0);
                }
                return Err(ALREADY_RUNNING);
            }
            runtime.instance.wait();
            if runtime.instance.owned {
                break;
            }
        }
        if !runtime.instance.owned {
            return Err(ALREADY_RUNNING);
        }
    }
    runtime.ensure(
        Service::Studio,
        &helper.with_file_name("Lummix Host.exe"),
        &http,
        modern,
    )?;
    runtime.ensure(Service::Wrist, &helper, &http, modern)?;
    runtime.ensure(Service::Media, &helper, &http, modern)?;
    Ok(runtime)
}

#[no_mangle]
pub extern "C" fn lummix_host_dpi() {
    // May return false when a manifest or test already configured this process.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// All UTF-16 spans are valid for their declared length. The class span is nul-terminated.
/// The token out pointer is writable, and the token must be closed on the caller's thread.
#[no_mangle]
pub unsafe extern "C" fn lummix_host_start(
    helper: PCWSTR,
    helper_len: usize,
    class: PCWSTR,
    can_reopen: u32,
    open: usize,
    modern_media: u32,
    token: *mut *mut c_void,
) -> u32 {
    if helper.is_null() || helper_len == 0 || class.is_null() || token.is_null() {
        return WINDOWS_ERROR;
    }
    *token = null_mut();
    let helper = std::ffi::OsString::from_wide(std::slice::from_raw_parts(helper, helper_len));
    let protocol = WindowProtocol {
        class,
        can_reopen,
        open,
    };
    match prepare(PathBuf::from(helper), protocol, modern_media != 0) {
        Ok(runtime) => {
            *token = Box::into_raw(Box::new(runtime)).cast();
            OWNED
        }
        Err(status) => status,
    }
}

/// A non-null token was returned by lummix_host_start and has not already been closed.
#[no_mangle]
pub unsafe extern "C" fn lummix_host_stop(token: *mut c_void) {
    if !token.is_null() {
        drop(Box::from_raw(token.cast::<Runtime>()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tiny_http::{Header, Response, Server};

    #[test]
    fn winhttp_preserves_encoded_studio_route_and_requires_markers() {
        let server = Server::http(("127.0.0.1", 0)).unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let fixture = thread::spawn(move || {
            for (route, body) in [
                ("/", "<script src=design-board.js>"),
                ("/Mixer%20preview.html", "<canvas id=\"lum-visualizer\">"),
            ] {
                let request = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.url(), route);
                request.respond(Response::from_string(body)).unwrap();
            }
        });
        let http = LoopbackClient::new().unwrap();
        assert!(Service::Studio.matches_at(&http, port, true).unwrap());
        fixture.join().unwrap();
    }

    #[test]
    fn winhttp_media_probe_requires_origin_protocol_and_all_capabilities() {
        let server = Server::http(("127.0.0.1", 0)).unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let fixture = thread::spawn(move || {
            let mut health = json!({
                "service": "lummix-media",
                "protocol": 2,
                "capabilities": [
                    "spotify-session-peak-v1", "mic-route-v1", "mixer-duck-v1",
                    "studio-preferences-v1", "mix-setup-alias-v1", "cable-restart-v1",
                    "spotify-detection-v2", "spotify-window-artwork-v1",
                ],
            });
            let cors = Header::from_bytes("Access-Control-Allow-Origin", "http://127.0.0.1:41835")
                .unwrap();
            for reply in [
                Response::from_string(health.to_string()).with_header(cors.clone()),
                Response::from_string(health.to_string()).with_header(cors.clone()),
                Response::from_string(health.to_string()),
            ] {
                let request = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.url(), "/lummix-health");
                assert!(request
                    .headers()
                    .iter()
                    .any(|header| header.field.equiv("Origin")
                        && header.value.as_str() == "http://127.0.0.1:41835"));
                request.respond(reply).unwrap();
            }
            health["capabilities"]
                .as_array_mut()
                .unwrap()
                .push(json!("spotify-process-mix-v1"));
            let request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            request
                .respond(Response::from_string(health.to_string()).with_header(cors))
                .unwrap();
        });
        let http = LoopbackClient::new().unwrap();
        assert!(Service::Media.matches_at(&http, port, false).unwrap());
        assert!(!Service::Media.matches_at(&http, port, true).unwrap());
        assert!(!Service::Media.matches_at(&http, port, false).unwrap());
        assert!(Service::Media.matches_at(&http, port, true).unwrap());
        fixture.join().unwrap();
    }

    #[test]
    fn startup_mutex_transfers_only_after_release() {
        let name: Vec<_> = format!("Local\\LummixRustHostTest-{}", std::process::id())
            .encode_utf16()
            .chain([0])
            .collect();
        let original = Instance::acquire(name.as_ptr()).unwrap();
        assert!(original.owned);
        let (release, released) = std::sync::mpsc::channel();
        let (contending, contention) = std::sync::mpsc::channel();
        let caller = thread::spawn(move || {
            let mut host = Instance::acquire(name.as_ptr()).unwrap();
            assert!(!host.owned);
            host.wait();
            assert!(!host.owned);
            contending.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
            host.wait();
            assert!(host.owned);
        });
        contention.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(original);
        release.send(()).unwrap();
        caller.join().unwrap();
    }
}
