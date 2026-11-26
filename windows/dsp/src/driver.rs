//! Own the pinned PortAudio library and pair its Windows initialization and termination on one thread.
use super::{devices, Api, Engine, OpenResult};
use std::ffi::{c_char, c_int, c_void, OsString};
use std::os::windows::ffi::OsStringExt;
use std::path::Path;
use std::ptr;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

// pa_win_wasapi.h, PaWasapiStreamInfo v1. The public x64 binary uses the
// Windows LLP64 layout. Both hooks are always null in shared event mode.
#[repr(C)]
struct WasapiSettings {
    size: u32,
    host: i32,
    version: u32,
    flags: u32,
    channel_mask: u32,
    output_hook: usize,
    input_hook: usize,
    priority: i32,
    category: i32,
    option: i32,
}
static SETTINGS: WasapiSettings = WasapiSettings {
    size: size_of::<WasapiSettings>() as u32,
    host: 13,
    version: 1,
    flags: 0x40, // paWinWasapiAutoConvert
    channel_mask: 0,
    output_hook: 0,
    input_hook: 0,
    priority: 0,
    category: 0,
    option: 0,
};
pub(super) fn settings() -> *const c_void {
    ptr::from_ref(&SETTINGS).cast()
}

type Call = unsafe extern "C" fn() -> c_int;
type Endpoint = unsafe extern "C" fn(c_int, *mut *mut c_void) -> c_int;

struct Functions {
    stream: Api,
    count: Call,
    endpoint: Endpoint,
    refresh: Option<Call>,
}

struct Module(HMODULE);
impl Drop for Module {
    fn drop(&mut self) {
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

fn own(path: Vec<u16>, ready: mpsc::SyncSender<Functions>, stop: mpsc::Receiver<()>) -> Option<()> {
    // Only load the full path supplied by the installation, never a name from PATH.
    let module = Module(
        unsafe {
            LoadLibraryExW(
                PCWSTR(path.as_ptr()),
                None,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .ok()?,
    );
    let lookup =
        |name: &std::ffi::CStr| unsafe { GetProcAddress(module.0, PCSTR(name.as_ptr().cast())) };
    // All signatures here correspond to the pinned PortAudio C ABI.
    macro_rules! export {
        ($name:expr) => {
            unsafe { std::mem::transmute(lookup($name)?) }
        };
    }
    let init: Call = export!(c"Pa_Initialize");
    let terminate: Call = export!(c"Pa_Terminate");
    let functions = Functions {
        stream: Api {
            device: export!(c"Pa_GetDeviceInfo"),
            host: export!(c"Pa_GetHostApiInfo"),
            open: export!(c"Pa_OpenStream"),
            start: export!(c"Pa_StartStream"),
            active: export!(c"Pa_IsStreamActive"),
            info: export!(c"Pa_GetStreamInfo"),
            abort: export!(c"Pa_AbortStream"),
            close: export!(c"Pa_CloseStream"),
        },
        count: export!(c"Pa_GetDeviceCount"),
        endpoint: export!(c"PaWasapi_GetIMMDevice"),
        refresh: lookup(c"PaWasapi_UpdateDeviceList")
            .map(|f| unsafe { std::mem::transmute::<_, Call>(f) }),
    };
    if unsafe { init() } != 0 {
        return None;
    }
    if ready.send(functions).is_ok() {
        let _ = stop.recv();
    }
    unsafe { terminate() };
    Some(())
}

pub(crate) struct Driver {
    api: Functions,
    pub(super) streams: Mutex<usize>,
    stop: mpsc::Sender<()>,
    owner: Option<JoinHandle<()>>,
}
impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lummix_driver_load(path: *const u16, size: usize) -> *mut Arc<Driver> {
    if path.is_null() || !(1..=32767).contains(&size) {
        return ptr::null_mut();
    }
    let path = unsafe { std::slice::from_raw_parts(path, size) };
    let os_path = OsString::from_wide(path);
    let file = Path::new(&os_path);
    if path.contains(&0)
        || !file.is_absolute()
        || file.file_name().and_then(|n| n.to_str()) != Some("libportaudio64bit.dll")
    {
        return ptr::null_mut();
    }
    let terminated = path.iter().copied().chain([0]).collect();
    let (ready, loaded) = mpsc::sync_channel(0);
    let (stop, waiting) = mpsc::channel();
    let Ok(owner) = thread::Builder::new()
        .name("Lummix audio devices".into())
        .spawn(move || {
            own(terminated, ready, waiting);
        })
    else {
        return ptr::null_mut();
    };
    match loaded.recv() {
        Ok(api) => Box::into_raw(Box::new(Arc::new(Driver {
            api,
            streams: Mutex::new(0),
            stop,
            owner: Some(owner),
        }))),
        Err(_) => {
            let _ = owner.join();
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lummix_driver_free(driver: *mut Arc<Driver>) {
    if !driver.is_null() {
        drop(unsafe { Box::from_raw(driver) });
    }
}

#[no_mangle]
pub unsafe extern "C" fn lummix_driver_pair(
    driver: &Arc<Driver>,
    input: *const c_char,
    ids: *const c_char,
) -> devices::Pair {
    let active = driver.streams.lock().unwrap();
    let api = &driver.api;
    // Device table refresh invalidates PortAudio-owned pointers. Allow it only while idle.
    let refresh = if *active == 0 { api.refresh } else { None };
    unsafe {
        devices::lummix_route_indexes(&api.stream, api.count, api.endpoint, refresh, input, ids)
    }
}

#[no_mangle]
pub unsafe extern "C" fn lummix_driver_stream(
    driver: &Arc<Driver>,
    engine: &Engine,
    mic: c_int,
    sink: c_int,
) -> OpenResult {
    let mut active = driver.streams.lock().unwrap();
    let result =
        unsafe { super::lummix_route_open(engine, &driver.api.stream, mic, sink, settings()) };
    if let Some(route) = unsafe { result.handle.as_mut() } {
        route.driver = Some(Arc::clone(driver));
        *active += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_x64_wasapi_options() {
        assert_eq!(size_of::<WasapiSettings>(), 56);
        assert_eq!(std::mem::offset_of!(WasapiSettings, output_hook), 24);
        assert_eq!(std::mem::offset_of!(WasapiSettings, option), 48);
        assert_eq!(SETTINGS.flags, 64);
    }
}
