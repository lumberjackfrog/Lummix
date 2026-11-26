use crate::{capture, engine::Engine};
use std::ffi::{c_char, CStr, CString};
use std::ptr;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    mpsc, Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

type Probe = unsafe extern "C" fn(*const c_char, *mut u32, *mut f64, *mut u32) -> i32;

pub struct Watch {
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
    status: Arc<AtomicU32>,
}
const CONNECTING: u32 = 0;
const NO_PLAYER: u32 = 1;
const SILENT: u32 = 2;
const RECEIVING: u32 = 3;
const DIRECT: u32 = 4;
const UNKNOWN: u32 = 5;
const AMBIGUOUS: u32 = 6;

fn run(engine: Engine, ids: CString, probe: Probe, rx: mpsc::Receiver<()>, status: Arc<AtomicU32>) {
    let mut active: Option<((u32, u64), capture::Capture)> = None;
    loop {
        let (mut pid, mut created, mut edition) = (0, 0.0, 0);
        let outcome = unsafe { probe(ids.as_ptr(), &mut pid, &mut created, &mut edition) };
        let identity = (pid, created.to_bits());
        if outcome != 1 || active.as_ref().is_some_and(|(key, _)| *key != identity) {
            active = None;
        }
        let state = match outcome {
            0 => NO_PLAYER,
            2 => AMBIGUOUS,
            3 => DIRECT,
            1 if pid != 0 && created.is_finite() && (edition == 1 || edition == 2) => {
                if active.is_none() {
                    active = capture::open(&engine, pid, created)
                        .ok()
                        .map(|c| (identity, c));
                }
                match active
                    .as_ref()
                    .map(|(_, c)| capture::lummix_capture_state(c))
                {
                    Some(0) => CONNECTING,
                    Some(1) => SILENT,
                    Some(2) => RECEIVING,
                    _ => {
                        active = None;
                        UNKNOWN
                    }
                }
            }
            _ => {
                active = None;
                UNKNOWN
            }
        };
        // One word publishes the state and edition from the same probe.
        status.store(
            state
                | if (1..=2).contains(&edition) {
                    edition << 8
                } else {
                    0
                },
            Ordering::Release,
        );
        let interval = if state == UNKNOWN { 2000 } else { 500 };
        match rx.recv_timeout(Duration::from_millis(interval)) {
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            _ => break,
        }
    }
}
#[no_mangle]
pub unsafe extern "C" fn lummix_watch_start(
    engine: &Engine,
    ids: *const c_char,
    probe: Option<Probe>,
) -> *mut Watch {
    let Some(probe) = probe else {
        return ptr::null_mut();
    };
    if ids.is_null() {
        return ptr::null_mut();
    }
    let ids = unsafe { CStr::from_ptr(ids) }.to_owned();
    if serde_json::from_slice::<Vec<String>>(ids.as_bytes()).map_or(true, |v| v.is_empty()) {
        return ptr::null_mut();
    }
    let engine = engine.clone();
    let status = Arc::new(AtomicU32::new(CONNECTING));
    let state = status.clone();
    let (stop, rx) = mpsc::channel();
    match thread::Builder::new()
        .name("Lummix source routing".into())
        .spawn(move || run(engine, ids, probe, rx, state))
    {
        Ok(worker) => Box::into_raw(Box::new(Watch {
            stop,
            thread: Some(worker),
            status,
        })),
        Err(_) => ptr::null_mut(),
    }
}
#[no_mangle]
pub extern "C" fn lummix_watch_status(w: &Watch) -> u32 {
    w.status.load(Ordering::Acquire)
}
#[no_mangle]
pub unsafe extern "C" fn lummix_watch_close(w: *mut Watch) {
    if w.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(w) });
}
impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
