//! Owned Windows audio route used by the standalone host.
use crate::{capture, engine, route, watch};
use serde_json::Value;
use std::ffi::{c_char, CString};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use engine::{Controls, Metrics};
pub type SourceProbe = unsafe extern "C" fn(*const c_char, *mut u32, *mut f64, *mut u32) -> i32;

pub fn inputs() -> Result<Vec<Value>, &'static str> {
    route::devices::inventory().map_err(|_| "route_unavailable")
}

/// Probe the packaged PortAudio runtime without opening a device.
pub fn verify_driver(library: &Path) -> Result<(), &'static str> {
    let path:Vec<_>=library.as_os_str().encode_wide().collect();
    for _ in 0..2 {
        let driver=NonNull::new(unsafe {route::driver::lummix_driver_load(path.as_ptr(),path.len())})
            .ok_or("PortAudio initialization failed")?;
        unsafe {route::driver::lummix_driver_free(driver.as_ptr());}
    }
    Ok(())
}

pub struct LiveRoute {
    stream: NonNull<route::Route>,
    watch: Option<Box<watch::Watch>>,
    engine: engine::Engine,
    high: bool,
    supported: bool,
}

// The host serializes status, configuration and closure. The PortAudio callback only
// touches its own callback state and the atomics/mutexes in Shared. The stream retains
// the DLL until Pa_CloseStream has joined its callback.
unsafe impl Send for LiveRoute {}

impl LiveRoute {
    pub fn open(library: &Path, mic_id: &str, render_ids: &[String], controls: Controls,
                supported: bool, probe: SourceProbe) -> Result<Self, &'static str> {
        if render_ids.is_empty() { return Err("cable_unavailable"); }
        let input = CString::new(mic_id).map_err(|_| "selected_input_unavailable")?;
        let ids = CString::new(serde_json::to_vec(render_ids).map_err(|_| "cable_unavailable")?)
            .map_err(|_| "cable_unavailable")?;
        let path: Vec<u16> = library.as_os_str().encode_wide().collect();
        let driver = NonNull::new(unsafe {route::driver::lummix_driver_load(path.as_ptr(), path.len())})
            .ok_or("route_start_failed")?;
        let driver = unsafe {Box::from_raw(driver.as_ptr())};
        let pair = unsafe {route::driver::lummix_driver_pair(&driver, input.as_ptr(), ids.as_ptr())};
        if pair.mic < 0 || pair.sink < 0 {return Err("restart_audio_helper");}
        let engine = Arc::new(engine::Shared::new(controls));
        let opened = unsafe {route::driver::lummix_driver_stream(&driver, &engine, pair.mic, pair.sink)};
        let stream = NonNull::new(opened.handle).ok_or("route_start_failed")?;
        let watch = supported.then(|| unsafe {
            NonNull::new(watch::lummix_watch_start(&engine, ids.as_ptr(), Some(probe)))
                .map(|watch| Box::from_raw(watch.as_ptr()))
        }).flatten();
        Ok(Self { stream, watch, engine, high: opened.high_latency != 0, supported })
    }

    pub fn configure(&mut self, controls: Controls) {unsafe {engine::lummix_engine_controls(&self.engine, controls);}}
    pub fn active(&self) -> bool {unsafe {route::lummix_route_active(self.stream.as_ref()) == 1}}
    pub fn latency(&self) -> (&'static str, [f64; 2]) {
        let latency = unsafe {route::lummix_route_latency(self.stream.as_ref())};
        (if self.high {"high"} else {"low"}, [latency.input, latency.output])
    }
    pub fn metrics(&self) -> Metrics {engine::lummix_engine_metrics(&self.engine)}
    pub fn source(&self) -> (&'static str, Option<&'static str>) {
        let Some(w) = &self.watch else {return (if self.supported {"unavailable"} else {"unsupported"}, None)};
        let status = watch::lummix_watch_status(w);
        let states = ["connecting","waiting_spotify","waiting_audio","receiving","direct_to_cable","unavailable","ambiguous"];
        (states.get((status & 255) as usize).copied().unwrap_or("unavailable"),
          match status >> 8 {1=>Some("desktop"),2=>Some("store"),_=>None})
    }
}
impl Drop for LiveRoute {
    fn drop(&mut self) {unsafe {route::lummix_route_close(self.stream.as_ptr());}}
}

/// Read-only support diagnostic. It attaches only to the verified player PID+creation
/// time, never opens a microphone or an output stream, and always releases capture.
pub fn check_capture(pid:u32, created:f64) -> Result<i32, &'static str> {
    let engine = Arc::new(engine::Shared::new(Controls{mic:1.,music:1.,depth:12.,flags:0}));
    let capture = capture::open(&engine,pid,created).map_err(|_| "capture_unavailable")?;
    let end = Instant::now() + Duration::from_secs(4);
    loop {
        let state = capture::lummix_capture_state(&capture);
        if !matches!(state, 0|1) || Instant::now()>=end {return Ok(state);}
        std::thread::sleep(Duration::from_millis(40));
    }
}
