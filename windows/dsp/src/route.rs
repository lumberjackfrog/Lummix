use crate::engine::{DuplexStage, Engine, Stereo, BLOCK};
use std::ffi::{c_int, c_ulong, c_void};
use std::ptr;
#[cfg(windows)]
#[path = "devices.rs"]
mod devices;

#[repr(C)]
pub struct Params {
    device: c_int,
    channels: c_int,
    format: c_ulong,
    latency: f64,
    host_settings: *const c_void,
}
#[repr(C)]
pub struct Device {
    version: c_int,
    name: *const u8,
    host: c_int,
    inputs: c_int,
    outputs: c_int,
    low_in: f64,
    low_out: f64,
    high_in: f64,
    high_out: f64,
    rate: f64,
}
#[repr(C)]
pub struct Host {
    version: c_int,
    api: c_int,
}
#[repr(C)]
pub struct Info {
    version: c_int,
    input_latency: f64,
    output_latency: f64,
    sample_rate: f64,
}
type Handle = *mut c_void;
type CallbackFn = unsafe extern "C" fn(
    *const f32,
    *mut Stereo,
    c_ulong,
    *const c_void,
    c_ulong,
    *mut c_void,
) -> c_int;

// Resolve these from the same PortAudio instance that mapped the selected IMMDevice IDs.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Api {
    device: unsafe extern "C" fn(c_int) -> *const Device,
    host: unsafe extern "C" fn(c_int) -> *const Host,
    open: unsafe extern "C" fn(
        *mut Handle,
        *const Params,
        *const Params,
        f64,
        c_ulong,
        c_ulong,
        CallbackFn,
        *mut c_void,
    ) -> c_int,
    start: unsafe extern "C" fn(Handle) -> c_int,
    active: unsafe extern "C" fn(Handle) -> c_int,
    info: unsafe extern "C" fn(Handle) -> *const Info,
    abort: unsafe extern "C" fn(Handle) -> c_int,
    close: unsafe extern "C" fn(Handle) -> c_int,
}

struct Callback {
    shared: Engine,
    stage: DuplexStage,
}
pub struct Route {
    api: Api,
    stream: Handle,
    _callback: Box<Callback>,
    frames: usize,
}
#[repr(C)]
pub struct OpenResult {
    pub handle: *mut Route,
    pub error: c_int,
    pub high_latency: c_int,
}
#[repr(C)]
pub struct Latency {
    pub input: f64,
    pub output: f64,
}

unsafe extern "C" fn render(
    input: *const f32,
    out: *mut Stereo,
    frames: c_ulong,
    _time: *const c_void,
    flags: c_ulong,
    user: *mut c_void,
) -> c_int {
    if out.is_null() {
        return 2;
    } // paAbort
    let cb = unsafe { &mut *user.cast::<Callback>() };
    let n = frames as usize;
    let source = if input.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(input, n) })
    };
    let output = unsafe { std::slice::from_raw_parts_mut(out, n) };
    cb.stage.render(&cb.shared, output, source, flags != 0);
    0 // paContinue
}

#[no_mangle]
pub unsafe extern "C" fn lummix_route_open(
    engine: &Engine,
    api: &Api,
    mic: c_int,
    sink: c_int,
    wasapi: *const c_void,
) -> OpenResult {
    let failed = |error| OpenResult {
        handle: ptr::null_mut(),
        error,
        high_latency: 0,
    };
    if mic < 0 || sink < 0 || wasapi.is_null() {
        return failed(-9996);
    } // paInvalidDevice
    let (Some(input), Some(output)) = (unsafe { (api.device)(mic).as_ref() }, unsafe {
        (api.device)(sink).as_ref()
    }) else {
        return failed(-9996);
    };
    if input.inputs < 1
        || output.outputs < 2
        || input.host != output.host
        || unsafe { (api.host)(input.host).as_ref() }.is_none_or(|h| h.api != 13)
    {
        return failed(-9996);
    }
    let controls = *engine.controls.lock().unwrap();
    let mut cb = Box::new(Callback {
        shared: engine.clone(),
        stage: DuplexStage::new(controls),
    });
    let mut last_error = -9985;
    // Ask fast shared-mode endpoints for a 5 ms callback. Devices may negotiate
    // a larger Windows buffer or decline; the two original modes remain available.
    let fast = [input.low_in, output.low_out]
        .into_iter()
        .all(|v| v > 0. && v <= 0.005);
    for (frames, high) in [(BLOCK / 2, false), (BLOCK, false), (BLOCK, true)] {
        if frames < BLOCK && !fast {
            continue;
        }
        let src = Params {
            device: mic,
            channels: 1,
            format: 1,
            latency: if high { input.high_in } else { input.low_in },
            host_settings: wasapi,
        };
        let dst = Params {
            device: sink,
            channels: 2,
            format: 1,
            latency: if high {
                output.high_out
            } else {
                output.low_out
            },
            host_settings: wasapi,
        };
        let mut stream = ptr::null_mut();
        let err = unsafe {
            (api.open)(
                &mut stream,
                &src,
                &dst,
                48000.0,
                frames as c_ulong,
                0,
                render,
                (&mut *cb as *mut Callback).cast(),
            )
        };
        if err != 0 {
            last_error = err;
            continue;
        }
        let err = unsafe { (api.start)(stream) };
        if err == 0 {
            let route = Route {
                api: *api,
                stream,
                _callback: cb,
                frames,
            };
            return OpenResult {
                handle: Box::into_raw(Box::new(route)),
                error: 0,
                high_latency: high as c_int,
            };
        }
        unsafe {
            (api.close)(stream);
        }
        last_error = err;
    }
    failed(last_error)
}
#[no_mangle]
pub unsafe extern "C" fn lummix_route_active(route: &Route) -> c_int {
    unsafe { (route.api.active)(route.stream) }
}
#[no_mangle]
pub extern "C" fn lummix_route_frames(route: &Route) -> usize {
    route.frames
}
#[no_mangle]
pub unsafe extern "C" fn lummix_route_latency(route: &Route) -> Latency {
    match unsafe { (route.api.info)(route.stream).as_ref() } {
        Some(i) => Latency {
            input: i.input_latency,
            output: i.output_latency,
        },
        None => Latency {
            input: 0.0,
            output: 0.0,
        },
    }
}
#[no_mangle]
pub unsafe extern "C" fn lummix_route_close(handle: *mut Route) -> c_int {
    if handle.is_null() {
        return 0;
    }
    let route = unsafe { Box::from_raw(handle) };
    unsafe {
        (route.api.abort)(route.stream);
        (route.api.close)(route.stream)
    }
}
