use crossbeam_queue::ArrayQueue;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const BLOCK: usize = 480;
pub type Stereo = [f32; 2];
type Block = [Stereo; BLOCK];

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Controls {
    pub mic: f64,
    pub music: f64,
    pub depth: f64,
    pub flags: u32, // mic mute, music mute, master mute, duck enabled
}

impl Controls {
    fn valid(self) -> bool {
        (0.0..=2.0).contains(&self.mic)
            && (0.0..=2.0).contains(&self.music)
            && (0.0..=24.0).contains(&self.depth)
            && self.flags < 16
    }
}

struct Packet {
    epoch: u64,
    signal: bool,
    samples: Block,
}

pub struct Bus {
    queue: ArrayQueue<Packet>,
    pub epoch: AtomicU64,
    started: Instant,
    last_signal: AtomicU64,
}

impl Bus {
    pub fn new() -> Self {
        Self {
            queue: ArrayQueue::new(6),
            epoch: AtomicU64::new(0),
            started: Instant::now(),
            last_signal: AtomicU64::new(0),
        }
    }
    pub fn reset(&self) -> u64 {
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        while self.queue.pop().is_some() {}
        self.last_signal.store(0, Ordering::Release);
        epoch
    }
    pub fn recent(&self) -> bool {
        let stamp = self.last_signal.load(Ordering::Acquire);
        stamp != 0 && self.millis().saturating_sub(stamp) < 1200
    }
    fn millis(&self) -> u64 {
        1 + self.started.elapsed().as_millis() as u64
    }
    fn pop(&self) -> Option<Packet> {
        self.next_packet().filter(|p| p.signal)
    }
    fn next_packet(&self) -> Option<Packet> {
        self.queue
            .pop()
            .filter(|p| p.epoch == self.epoch.load(Ordering::Acquire))
    }
}

pub struct Packetizer {
    data: Block,
    frames: usize,
    signal: bool,
    epoch: u64,
}
impl Packetizer {
    pub fn new(epoch: u64) -> Self {
        Self {
            data: [[0.0; 2]; BLOCK],
            frames: 0,
            signal: false,
            epoch,
        }
    }
    pub fn push(&mut self, bus: &Bus, samples: Option<&[[i16; 2]]>, frames: usize, flags: u32) {
        if flags & 1 != 0 {
            self.frames = 0;
            self.signal = false;
        }
        for i in 0..frames {
            let pair = samples.map_or([0; 2], |s| s[i]);
            self.signal |= pair[0].unsigned_abs() > 4 || pair[1].unsigned_abs() > 4;
            self.data[self.frames] = [pair[0] as f32 / 32768.0, pair[1] as f32 / 32768.0];
            self.frames += 1;
            if self.frames == BLOCK {
                if self.signal {
                    bus.last_signal.store(bus.millis(), Ordering::Release);
                }
                bus.queue.force_push(Packet {
                    epoch: self.epoch,
                    signal: self.signal,
                    samples: self.data,
                });
                self.frames = 0;
                self.signal = false;
            }
        }
    }
}

pub struct Shared {
    pub controls: Mutex<Controls>,
    pub bus: Bus,
    pub rms: AtomicU32,
    pub reduction: AtomicU32,
    pub music: AtomicBool,
    pub xruns: AtomicU64,
}
impl Shared {
    fn new(c: Controls) -> Self {
        Self {
            controls: Mutex::new(c),
            bus: Bus::new(),
            rms: 0.into(),
            reduction: 0.into(),
            music: false.into(),
            xruns: 0.into(),
        }
    }
}
#[derive(Default)]
struct Envelope {
    amount: f64,
    target: f64,
    rate: f64,
    on_since: Option<f64>,
    last_voice: Option<f64>,
    last_tick: Option<f64>,
    open: bool,
}
impl Envelope {
    fn step(&mut self, now: f64, rms: f64, enabled: bool, depth: f64) -> f64 {
        if enabled && rms >= if self.open { 0.004 } else { 0.01 } {
            let since = self.on_since.get_or_insert(now);
            if now - *since >= 0.035 {
                self.open = true;
                self.last_voice = Some(now);
            }
        } else {
            self.on_since = None;
        }
        if !enabled || self.last_voice.is_none_or(|last| now - last >= 0.450) {
            self.open = false;
        }
        let target = if self.open { depth } else { 0.0 };
        let dt = self
            .last_tick
            .map_or(0.0, |last| (now - last).clamp(0.0, 0.080));
        self.last_tick = Some(now);
        if target != self.target {
            self.rate =
                (target - self.amount).abs() / if target > self.amount { 0.35 } else { 1.0 };
            self.target = target;
        }
        self.amount += (target - self.amount).clamp(-dt * self.rate, dt * self.rate);
        self.amount
    }
}
struct Fader {
    current: f64,
    target: f64,
    left: usize,
    ramp: [f32; BLOCK],
}
impl Fader {
    fn new(value: f64) -> Self {
        Self {
            current: value,
            target: value,
            left: 0,
            ramp: [0.0; BLOCK],
        }
    }
    fn gain(&mut self, target: f64, muted: bool, frames: usize) -> (*const f32, f32) {
        if muted {
            self.current = 0.0;
            self.target = 0.0;
            self.left = 0;
            return (std::ptr::null(), 0.0);
        }
        if self.target != target {
            self.target = target;
            self.left = 720;
        }
        if self.left == 0 {
            return (std::ptr::null(), target as f32);
        }
        let n = frames.min(self.left);
        let step = (target - self.current) / self.left as f64;
        for (i, gain) in self.ramp[..n].iter_mut().enumerate() {
            *gain = (self.current + (i + 1) as f64 * step) as f32;
        }
        self.current += n as f64 * step;
        self.left -= n;
        if self.left == 0 {
            self.current = target;
            self.ramp[n..frames].fill(target as f32);
        }
        (self.ramp.as_ptr(), 0.0)
    }
}

pub struct Stage {
    controls: Controls,
    mic: Fader,
    music: Fader,
    envelope: Envelope,
    limit: f64,
    duck: f64,
    clock: f64,
}

/// Windows capture delivers 480-frame packets; duplex callbacks can be shorter.
pub struct DuplexStage {
    stage: Stage,
    packet: Option<Packet>,
    offset: usize,
}
impl DuplexStage {
    pub fn new(c: Controls) -> Self {
        Self {
            stage: Stage::new(c),
            packet: None,
            offset: 0,
        }
    }
    pub fn render(
        &mut self,
        shared: &Shared,
        out: &mut [Stereo],
        mic: Option<&[f32]>,
        status: bool,
    ) {
        if status {
            shared.xruns.fetch_add(1, Ordering::Relaxed);
        }
        if out.len() == BLOCK && self.packet.is_none() {
            let packet = shared.bus.pop();
            self.stage.render_inputs(
                shared,
                out,
                mic,
                packet.as_ref().map(|p| p.samples.as_slice()),
            );
            return;
        }
        let mut at = 0;
        while at < out.len() {
            if self
                .packet
                .as_ref()
                .is_some_and(|p| p.epoch != shared.bus.epoch.load(Ordering::Acquire))
            {
                self.packet = None;
            }
            if self.packet.is_none() {
                self.packet = shared.bus.next_packet();
                self.offset = 0;
            }
            let n = (BLOCK - self.offset).min(out.len() - at);
            let music = self
                .packet
                .as_ref()
                .filter(|p| p.signal)
                .map(|p| &p.samples[self.offset..self.offset + n]);
            self.stage.render_inputs(
                shared,
                &mut out[at..at + n],
                mic.map(|m| &m[at..at + n]),
                music,
            );
            at += n;
            self.offset += n;
            if self.offset == BLOCK {
                self.packet = None;
            }
        }
    }
}
impl Stage {
    pub fn new(c: Controls) -> Self {
        Self {
            controls: c,
            mic: Fader::new(c.mic),
            music: Fader::new(c.music),
            envelope: Envelope::default(),
            limit: 1.0,
            duck: 1.0,
            clock: 0.0,
        }
    }
    fn render_inputs(
        &mut self,
        shared: &Shared,
        out: &mut [Stereo],
        input: Option<&[f32]>,
        music: Option<&[Stereo]>,
    ) {
        // Only the UI writer can contend. An update then takes effect on the following block.
        if let Ok(value) = shared.controls.try_lock() {
            self.controls = *value;
        }
        let c = self.controls;
        let mic_muted = c.flags & 5 != 0;
        let music_muted = c.flags & 6 != 0;
        let n = out.len();
        let (mr, mg) = self.mic.gain(c.mic, mic_muted, n);
        let (pr, pg) = self.music.gain(c.music, music_muted, n);
        let rms = input.map_or(0.0, |s| {
            (s.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / n as f64).sqrt()
        });
        self.clock += n as f64 / 48000.0;
        let active = c.flags & 8 != 0
            && !mic_muted
            && !music_muted
            && c.mic > 0.0
            && input.is_some()
            && music.is_some();
        let reduction = self.envelope.step(self.clock, rms, active, c.depth);
        let duck = if reduction == 0.0 {
            1.0
        } else {
            10_f64.powf(-reduction / 20.0)
        };
        shared.rms.store((rms as f32).to_bits(), Ordering::Relaxed);
        shared
            .reduction
            .store((reduction as f32).to_bits(), Ordering::Relaxed);
        shared.music.store(music.is_some(), Ordering::Relaxed);
        if c.flags & 4 != 0 {
            out.fill([0.0; 2]);
            self.limit = 1.0;
        } else {
            let mic = input
                .filter(|_| !mic_muted)
                .map_or(std::ptr::null(), |v| v.as_ptr());
            let music = music
                .filter(|_| !music_muted)
                .map_or(std::ptr::null(), |p| p.as_ptr());
            self.limit = unsafe {
                super::lummix_mix_f32(
                    n,
                    out.as_mut_ptr(),
                    mic,
                    music,
                    mr,
                    mg,
                    pr,
                    pg,
                    self.duck,
                    duck,
                    self.limit,
                    0.98,
                    1.0 / 3840.0,
                )
            };
        }
        self.duck = duck;
    }
}

pub type Engine = Arc<Shared>;

#[cfg(test)]
mod duplex_tests {
    use super::*;

    #[test]
    fn fractional_callbacks_preserve_packet_order_and_drop_retired_audio() {
        let controls = Controls {
            mic: 1.,
            music: 1.,
            depth: 12.,
            flags: 0,
        };
        let shared = Shared::new(controls);
        let mut route = DuplexStage::new(controls);
        let mut source = Packetizer::new(shared.bus.reset());
        let a = std::array::from_fn::<_, BLOCK, _>(|i| [100 + i as i16, -100 - i as i16]);
        let b = [[8000, -8000]; BLOCK];
        source.push(&shared.bus, Some(&a), BLOCK, 0);
        source.push(&shared.bus, Some(&b), BLOCK, 0);
        let mut all = [[0.; 2]; 700];
        let (first, rest) = all.split_at_mut(147);
        route.render(&shared, first, None, false);
        route.render(&shared, rest, None, false);
        for (frame, value) in all.iter().zip(a.iter().chain(b.iter())) {
            assert_eq!(*frame, value.map(|v| v as f32 / 32768.));
        }
        shared.bus.reset();
        let mut silence = [[1.; 2]; 239];
        route.render(&shared, &mut silence, None, false);
        assert!(silence.iter().all(|x| *x == [0.; 2]));
    }
}
#[no_mangle]
pub extern "C" fn lummix_engine_new(c: Controls) -> *mut Engine {
    if !c.valid() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(Arc::new(Shared::new(c))))
}
#[no_mangle]
pub unsafe extern "C" fn lummix_engine_free(engine: *mut Engine) {
    if !engine.is_null() {
        drop(unsafe { Box::from_raw(engine) });
    }
}
#[no_mangle]
pub unsafe extern "C" fn lummix_engine_controls(engine: &Engine, c: Controls) -> bool {
    if !c.valid() {
        return false;
    }
    *engine.controls.lock().unwrap() = c;
    true
}
#[repr(C)]
pub struct Metrics {
    rms: f32,
    reduction: f32,
    music: u32,
    xruns: u64,
}
#[no_mangle]
pub extern "C" fn lummix_engine_metrics(engine: &Engine) -> Metrics {
    Metrics {
        rms: f32::from_bits(engine.rms.load(Ordering::Relaxed)),
        reduction: f32::from_bits(engine.reduction.load(Ordering::Relaxed)),
        music: engine.music.load(Ordering::Relaxed) as u32,
        xruns: engine.xruns.load(Ordering::Relaxed),
    }
}
// Owned output is used by diagnostic capture, which never competes with a live route.
#[no_mangle]
pub extern "C" fn lummix_engine_read(engine: &Engine, out: &mut Block) -> bool {
    if let Some(p) = engine.bus.pop() {
        *out = p.samples;
        true
    } else {
        false
    }
}

#[no_mangle]
pub extern "C" fn lummix_stage_new(engine: &Engine) -> *mut Stage {
    Box::into_raw(Box::new(Stage::new(*engine.controls.lock().unwrap())))
}
#[no_mangle]
pub unsafe extern "C" fn lummix_stage_free(stage: *mut Stage) {
    if !stage.is_null() {
        drop(unsafe { Box::from_raw(stage) });
    }
}
#[no_mangle]
pub unsafe extern "C" fn lummix_stage_render(
    stage: &mut Stage,
    engine: &Engine,
    n: usize,
    out: *mut Stereo,
    mic: *const f32,
    music: *const Stereo,
) -> f64 {
    if n == 0 || out.is_null() {
        return stage.limit;
    }
    for first in (0..n).step_by(BLOCK) {
        let count = (n - first).min(BLOCK);
        let dst = unsafe { std::slice::from_raw_parts_mut(out.add(first), count) };
        let voice =
            (!mic.is_null()).then(|| unsafe { std::slice::from_raw_parts(mic.add(first), count) });
        let audio = (!music.is_null())
            .then(|| unsafe { std::slice::from_raw_parts(music.add(first), count) });
        stage.render_inputs(engine, dst, voice, audio);
    }
    stage.limit
}
