use std::slice;

#[no_mangle]
pub extern "C" fn lummix_dsp_abi() -> u32 {
    1
}

unsafe fn optional_input<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if ptr.is_null() {
        &[]
    } else {
        // The caller retains each input buffer until the render call returns.
        unsafe { slice::from_raw_parts(ptr, len) }
    }
}

/// Render interleaved stereo into an existing buffer. The returned linked limiter
/// gain belongs to this stream and is passed back on its next block.
///
/// # Safety
/// For a nonempty block, output must hold `frames` aligned stereo pairs and be
/// exclusively writable. Each non-null input must hold `frames` initialized
/// elements of its stated shape. Music has two channels, mic and ramps have one.
#[no_mangle]
pub unsafe extern "C" fn lummix_mix_f32(
    frames: usize,
    output: *mut [f32; 2],
    mic: *const f32,
    music: *const [f32; 2],
    mic_ramp: *const f32,
    mic_gain: f32,
    music_ramp: *const f32,
    music_gain: f32,
    duck_start: f64,
    duck_end: f64,
    mut limiter: f64,
    ceiling: f64,
    release_step: f64,
) -> f64 {
    if frames == 0 {
        return limiter;
    }
    let out = unsafe { slice::from_raw_parts_mut(output, frames) };
    let mic = unsafe { optional_input(mic, frames) };
    let music = unsafe { optional_input(music, frames) };
    let mic_ramp = unsafe { optional_input(mic_ramp, frames) };
    let music_ramp = unsafe { optional_input(music_ramp, frames) };
    let duck_step = ((duck_end - duck_start) / frames as f64) as f32;
    let ceiling_f32 = ceiling as f32;

    for (i, target) in out.iter_mut().enumerate() {
        let voice = mic.get(i).map_or(0.0, |sample| {
            sample * mic_ramp.get(i).copied().unwrap_or(mic_gain)
        });
        let [mut left, mut right] = [voice; 2];
        if let Some(pair) = music.get(i) {
            let gain = music_ramp.get(i).copied().unwrap_or(music_gain);
            let duck = (i + 1) as f32 * duck_step + duck_start as f32;
            left += (pair[0] * gain) * duck;
            right += (pair[1] * gain) * duck;
        }
        if !left.is_finite() {
            left = 0.0;
        }
        if !right.is_finite() {
            right = 0.0;
        }
        let peak = left.abs().max(right.abs()) as f64;
        let safe_gain = if peak > ceiling { ceiling / peak } else { 1.0 };
        limiter = safe_gain.min(limiter + release_step);
        if limiter < 1.0 {
            left = ((left as f64 * limiter) as f32).clamp(-ceiling_f32, ceiling_f32);
            right = ((right as f64 * limiter) as f32).clamp(-ceiling_f32, ceiling_f32);
        }
        *target = [left, right];
    }
    limiter
}
