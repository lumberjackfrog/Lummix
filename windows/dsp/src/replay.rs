//! QA-only injection at the same packet boundary as IAudioCaptureClient::GetBuffer.
use crate::engine::{Engine, Packetizer};
pub struct Source {
    engine: Engine,
    packetizer: Packetizer,
}
#[no_mangle]
pub extern "C" fn lummix_replay_source(engine: &Engine) -> *mut Source {
    Box::into_raw(Box::new(Source {
        engine: engine.clone(),
        packetizer: Packetizer::new(engine.bus.reset()),
    }))
}
#[no_mangle]
pub unsafe extern "C" fn lummix_replay_packet(
    src: &mut Source,
    data: *const [i16; 2],
    n: usize,
    flags: u32,
) {
    let pcm = if data.is_null() || flags & 2 != 0 {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(data, n) })
    };
    src.packetizer.push(&src.engine.bus, pcm, n, flags)
}
#[no_mangle]
pub unsafe extern "C" fn lummix_replay_free(src: *mut Source) {
    if !src.is_null() {
        drop(unsafe { Box::from_raw(src) });
    }
}
