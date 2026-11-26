use crate::engine::{Engine, Packetizer};
use std::mem::ManuallyDrop;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;
use std::sync::{
    atomic::{AtomicI32, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use windows::core::{implement, Error, Interface, Result, HRESULT, PROPVARIANT};
use windows::Win32::Foundation::{E_FAIL, FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, IAgileObject, IAgileObject_Impl, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::*;

fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}
fn owned(handle: HANDLE) -> OwnedHandle {
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}
fn event(manual: bool) -> Result<OwnedHandle> {
    Ok(owned(unsafe { CreateEventW(None, manual, false, None)? }))
}

#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Completion {
    done: Arc<OwnedHandle>,
    _params: Arc<AUDIOCLIENT_ACTIVATION_PARAMS>,
}
impl IAgileObject_Impl for Completion_Impl {}
impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(&self, _: Option<&IActivateAudioInterfaceAsyncOperation>) -> Result<()> {
        unsafe { SetEvent(raw(&self.done)) }
    }
}
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

fn wait(
    stop: &OwnedHandle,
    process: &OwnedHandle,
    signal: &OwnedHandle,
    timeout: u32,
) -> Result<bool> {
    let wake =
        unsafe { WaitForMultipleObjects(&[raw(stop), raw(process), raw(signal)], false, timeout) };
    if wake.0 == WAIT_OBJECT_0.0 + 2 {
        Ok(true)
    } else if wake.0 == WAIT_OBJECT_0.0 || wake.0 == WAIT_OBJECT_0.0 + 1 {
        Ok(false)
    } else if wake == WAIT_TIMEOUT {
        Err(Error::from_hresult(HRESULT(0x800705B4u32 as i32)))
    } else {
        Err(Error::from_win32())
    }
}
fn capture(
    pid: u32,
    process: OwnedHandle,
    stop: Arc<OwnedHandle>,
    engine: Engine,
    epoch: u64,
    status: &AtomicI32,
) -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let _apartment = Apartment;
        let done = Arc::new(event(true)?);
        let params = Arc::new(AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        });
        // VT_BLOB borrows this allocation. The completion object keeps it alive even on cancellation.
        let mut value = std::mem::zeroed::<windows_core::imp::PROPVARIANT>();
        value.Anonymous.Anonymous.vt = 65;
        value.Anonymous.Anonymous.Anonymous.blob = windows_core::imp::BLOB {
            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: Arc::as_ptr(&params).cast::<u8>().cast_mut(),
        };
        let variant = ManuallyDrop::new(PROPVARIANT::from_raw(value));
        let handler: IActivateAudioInterfaceCompletionHandler = Completion {
            done: done.clone(),
            _params: params,
        }
        .into();
        let op = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*variant),
            &handler,
        )?;
        if !wait(&stop, &process, &done, 5000)? {
            return Ok(());
        }
        let mut hr = HRESULT(0);
        let mut object = None;
        op.GetActivateResult(&mut hr, &mut object)?;
        hr.ok()?;
        let client: IAudioClient = object.ok_or_else(|| Error::from_hresult(E_FAIL))?.cast()?;
        let fmt = WAVEFORMATEX {
            wFormatTag: 1,
            nChannels: 2,
            nSamplesPerSec: 48000,
            nAvgBytesPerSec: 192000,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            0,
            0,
            &fmt,
            None,
        )?;
        let source: IAudioCaptureClient = client.GetService()?;
        let audio = event(false)?;
        client.SetEventHandle(raw(&audio))?;
        client.Start()?;
        status.store(1, Ordering::Release);
        let mut pack = Packetizer::new(epoch);
        let reading = (|| -> Result<()> {
            while wait(&stop, &process, &audio, INFINITE)? {
                while source.GetNextPacketSize()? != 0
                    && WaitForSingleObject(raw(&stop), 0) == WAIT_TIMEOUT
                {
                    let (mut data, mut frames, mut flags) = (ptr::null_mut(), 0, 0);
                    source.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                    let samples = if flags & 2 != 0 || data.is_null() {
                        None
                    } else {
                        Some(std::slice::from_raw_parts(
                            data.cast::<[i16; 2]>(),
                            frames as usize,
                        ))
                    };
                    pack.push(&engine.bus, samples, frames as usize, flags);
                    source.ReleaseBuffer(frames)?;
                }
            }
            Ok(())
        })();
        let ended = client.Stop();
        reading.and(ended)
    }
}

pub struct Capture {
    stop: Arc<OwnedHandle>,
    worker: Option<JoinHandle<()>>,
    state: Arc<AtomicI32>,
    engine: Engine,
}
pub fn open(engine: &Engine, pid: u32, created: f64) -> Result<Capture> {
    if pid == 0 || !created.is_finite() {
        return Err(Error::from_hresult(E_FAIL));
    }
    let process = owned(unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )?
    });
    let (mut t, mut end, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe {
        GetProcessTimes(raw(&process), &mut t, &mut end, &mut kernel, &mut user)?;
    }
    let ticks = ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    if ((ticks as f64 / 10_000_000.0 - 11_644_473_600.0) - created).abs() > 0.001 {
        return Err(Error::from_hresult(E_FAIL));
    }
    let signal = Arc::new(event(true)?);
    let state = Arc::new(AtomicI32::new(0));
    let (stop, output, status) = (signal.clone(), engine.clone(), state.clone());
    let epoch = engine.bus.reset();
    let worker = thread::Builder::new()
        .name("Lummix process audio".into())
        .spawn(move || {
            let result = capture(pid, process, stop, output, epoch, &status);
            status.store(
                result.map(|()| 3).unwrap_or_else(|e| e.code().0),
                Ordering::Release,
            );
        })
        .map_err(|_| Error::from_hresult(E_FAIL))?;
    Ok(Capture {
        stop: signal,
        worker: Some(worker),
        state,
        engine: engine.clone(),
    })
}
#[no_mangle]
pub extern "C" fn lummix_capture_open(engine: &Engine, pid: u32, created: f64) -> *mut Capture {
    open(engine, pid, created).map_or(ptr::null_mut(), |c| Box::into_raw(Box::new(c)))
}
#[no_mangle]
pub extern "C" fn lummix_capture_state(cap: &Capture) -> i32 {
    match cap.state.load(Ordering::Acquire) {
        1 if cap.engine.bus.recent() => 2,
        n => n,
    }
}
#[no_mangle]
pub unsafe extern "C" fn lummix_capture_close(handle: *mut Capture) {
    if handle.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(handle) });
}
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = unsafe { SetEvent(raw(&self.stop)) };
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.engine.bus.reset();
    }
}
