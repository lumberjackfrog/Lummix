use std::cell::RefCell;

const MISSING: i32 = -1;
const MAX_TEXT: usize = 400_000;
const SAVED: [u32; 7] = [
    0x232126, 0x37353b, 0x46434b, 0xf6f4f8, 0x7a0aa6, 0xcc91e3, 0xff7474,
];
const DARK: [u32; 5] = [0x12141b, 0x191b24, 0x272a38, 0xffffff, 0xefa4df];
const LIGHT: [u32; 5] = [0xedefef, 0xf7f8fa, 0xe4e8ef, 0x000000, 0x922870];

thread_local! {
    // JS writes UTF-8 bytes into this buffer and parses them in the same call stack.
    static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

#[no_mangle]
pub extern "C" fn appearance_buffer(length: usize) -> *mut u8 {
    if length >= MAX_TEXT {
        return std::ptr::null_mut();
    }
    INPUT.with_borrow_mut(|buffer| {
        buffer.resize(length, 0);
        buffer.as_mut_ptr()
    })
}

fn normalize(kind: u32, text: &[u8]) -> i32 {
    match kind {
        0 if matches!(text.len(), 6 | 8) && text.iter().all(u8::is_ascii_hexdigit) => {
            let rgb = std::str::from_utf8(&text[..6]).expect("ASCII hex");
            i32::from_str_radix(rgb, 16).unwrap_or(MISSING)
        }
        1 => match text {
            b"false" => 0,
            b"true" => 1,
            _ => MISSING,
        },
        2 => match text {
            b"0" => 0,
            b"5" => 5,
            b"10" => 10,
            b"20" => 20,
            _ => MISSING,
        },
        3 => match text {
            b"album" => 0,
            b"custom" => 1,
            _ => MISSING,
        },
        4 | 5 => {
            let Some(payload) = text.strip_prefix(b"data:image/jpeg;base64,") else {
                return MISSING;
            };
            if kind == 5 && !payload.starts_with(b"/9j/") {
                return MISSING;
            }
            // Legacy settings used the JPEG prefix as their migration test.
            if kind == 5 {
                return 1;
            }
            let encoded = payload.iter().take_while(|&&byte| byte != b'=').count();
            let padding = &payload[encoded..];
            if encoded > 0
                && padding.len() <= 2
                && padding.iter().all(|&byte| byte == b'=')
                && payload[..encoded]
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"+/".contains(byte))
            {
                1
            } else {
                MISSING
            }
        }
        _ => MISSING,
    }
}

#[no_mangle]
pub extern "C" fn appearance_normalize(kind: u32, length: usize) -> i32 {
    INPUT.with_borrow(|buffer| {
        buffer
            .get(..length)
            .map_or(MISSING, |text| normalize(kind, text))
    })
}

#[no_mangle]
pub extern "C" fn appearance_default(family: usize, field: usize) -> i32 {
    let palette: &[u32] = match family {
        0 => &SAVED,
        1 => &DARK,
        2 => &LIGHT,
        _ => &[],
    };
    palette.get(field).map_or(MISSING, |&value| value as i32)
}

#[no_mangle]
pub extern "C" fn appearance_selection(
    follow: bool,
    pinned: bool,
    wrist: bool,
    wrist_album: bool,
    wrist_custom: bool,
    host_base: bool,
    host_album: bool,
) -> u32 {
    // Low byte identifies the palette and the next byte its user-facing source.
    let (palette, source) = if !follow {
        (0, 0)
    } else if pinned {
        (
            if wrist && wrist_custom {
                1
            } else if host_base {
                3
            } else {
                4
            },
            1,
        )
    } else if wrist {
        (1, if wrist_album { 2 } else { 3 })
    } else if host_album {
        (2, 2)
    } else if host_base {
        (3, 3)
    } else {
        (4, 4)
    };
    palette | (source << 8)
}

#[no_mangle]
pub extern "C" fn appearance_background(mode: i32, has_image: bool, current: i32) -> i32 {
    match mode {
        0 => 0,
        1 if has_image => 1,
        _ => current,
    }
}

fn luminance(rgb: u32) -> f64 {
    let linear = |shift: u32| {
        let value = f64::from((rgb >> shift) & 255) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(16) + 0.7152 * linear(8) + 0.0722 * linear(0)
}

fn contrast(rgb: u32, background: u32) -> f64 {
    let (a, b) = (luminance(rgb), luminance(background));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn minimum(rgb: u32, surfaces: &[u32]) -> f64 {
    surfaces
        .iter()
        .map(|&surface| contrast(rgb, surface))
        .fold(f64::INFINITY, f64::min)
}

fn blend(from: u32, to: u32, fraction: f64) -> u32 {
    [16, 8, 0].into_iter().fold(0, |value, shift| {
        let a = f64::from((from >> shift) & 255);
        let b = f64::from((to >> shift) & 255);
        value | (((a + (b - a) * fraction).round() as u32) << shift)
    })
}

const BLACK: u32 = 0;
const WHITE: u32 = 0xffffff;

fn legible_stroke(source: u32, surfaces: &[u32], threshold: f64) -> u32 {
    let mut best = source;
    let mut best_score = minimum(source, surfaces);
    if best_score >= threshold {
        return source;
    }
    for step in 1..=32 {
        for extreme in [BLACK, WHITE] {
            let color = blend(source, extreme, f64::from(step) / 32.0);
            let score = minimum(color, surfaces);
            if score >= threshold {
                return color;
            }
            if score > best_score {
                best = color;
                best_score = score;
            }
        }
    }
    best
}

fn text_on(rgb: u32) -> u32 {
    if luminance(rgb) > 0.179 {
        BLACK
    } else {
        WHITE
    }
}

#[no_mangle]
pub extern "C" fn appearance_colors(
    dark: u32,
    mid: u32,
    hi: u32,
    text: u32,
    accent: u32,
    field: u32,
) -> u32 {
    let surfaces = [dark, mid, hi];
    if field == 1 {
        return text_on(accent);
    }
    if field == 0 {
        return if minimum(text, &surfaces) >= 4.5 {
            text
        } else if minimum(WHITE, &surfaces) >= minimum(BLACK, &surfaces) {
            WHITE
        } else {
            BLACK
        };
    }
    let control = legible_stroke(accent, &surfaces, 3.1);
    match field {
        2 => control,
        3 => legible_stroke(blend(dark, mid, 0.3), &[control], 4.5),
        4 => text_on(control),
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn appearance_dark(resolved_ink: u32) -> bool {
    luminance(resolved_ink) > 0.45
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_xsoverlay_hex_legacy_preference_and_background_rules() {
        assert_eq!(normalize(0, b"CD9011aF"), 0xcd9011);
        assert_eq!(normalize(0, b"ab0x11"), MISSING);
        assert_eq!(normalize(1, b"false"), 0);
        assert_eq!(normalize(1, b"TRUE"), MISSING);
        assert_eq!(normalize(2, b"00"), MISSING);
        assert_eq!(normalize(2, b"0"), 0);
        assert_eq!(normalize(3, b"custom"), 1);
        assert_eq!(normalize(4, b"data:image/jpeg;base64,aa=="), 1);
        assert_eq!(normalize(4, b"data:image/jpeg;base64,a=b"), MISSING);
        assert_eq!(normalize(5, b"data:image/jpeg;base64,aa=="), MISSING);
        assert_eq!(appearance_background(1, false, 0), 0);
        assert_eq!(appearance_background(1, true, 0), 1);
        assert_eq!(appearance_background(0, true, 1), 0);
    }

    #[test]
    fn wrist_pinned_and_album_precedence_matches_desktop_preference() {
        assert_eq!(
            appearance_selection(true, true, true, true, true, true, true),
            0x101
        );
        assert_eq!(
            appearance_selection(true, true, true, true, false, true, true),
            0x103
        );
        assert_eq!(
            appearance_selection(true, false, true, false, false, true, true),
            0x301
        );
        assert_eq!(
            appearance_selection(true, false, false, false, false, true, true),
            0x202
        );
        assert_eq!(
            appearance_selection(false, false, true, true, false, false, false),
            0
        );
        assert_eq!(
            appearance_selection(true, false, false, false, false, false, false),
            0x404
        );
    }

    #[test]
    fn saved_light_album_and_mixed_palettes_match_the_previous_renderer_exactly() {
        let samples = [
            (
                [0x232126, 0x37353b, 0x46434b, 0xf6f4f8, 0x7a0aa6],
                [0xf6f4f8, 0xffffff, 0xb87dd0, 0x29272c, 0],
            ),
            (
                [0xeff0f1, 0xf4f5fd, 0xf7fafa, 0xf1fff0, 0x93a5ac],
                [0, 0, 0x77868c, 0x171717, 0],
            ),
            (
                [0x130f09, 0x6a2e28, 0x915441, 0xfff2be, 0xff6b36],
                [0xfff2be, 0, 0xffa788, 0x2d1812, 0],
            ),
            (
                [0x1a191a, 0x808283, 0xe9dca0, 0x7e1a51, 0xb58278],
                [0xffffff, 0, 0x664944, 0xc7c7c8, 0xffffff],
            ),
        ];
        for ([dark, mid, hi, ink, accent], expected) in samples {
            let actual = std::array::from_fn(|field| {
                appearance_colors(dark, mid, hi, ink, accent, field as u32)
            });
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn preserves_palette_contrast_and_direction_tie_break() {
        let color =
            |field| appearance_colors(0x151515, 0x252525, 0x343434, 0x292929, 0x303030, field);
        assert_eq!(color(0), WHITE);
        assert_eq!(color(1), WHITE);
        assert!(appearance_dark(color(0)));
        let minimum_stroke = minimum(color(2), &[0x151515, 0x252525, 0x343434]);
        assert!(minimum_stroke >= 3.1);
        assert!(contrast(color(2), color(3)) >= 4.5);
        assert_eq!(color(4), BLACK);

        let light = appearance_colors(0xededef, 0xf7f8fa, 0xe4e8ef, 0xffffff, 0x922870, 0);
        assert_eq!(light, BLACK);
        assert!(!appearance_dark(light));
    }
}
