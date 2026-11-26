use base64::{engine::general_purpose::STANDARD, Engine};
use image_webp::WebPDecoder;
use serde_json::{json, Value};
use std::{
    ffi::{c_char, CStr},
    io::Cursor,
    ptr::{self, null},
    sync::Mutex,
};
use windows::{
    core::{w, Interface, PCWSTR, PROPVARIANT, PWSTR, VARIANT},
    Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        Graphics::Imaging::*,
        System::Com::{
            CoCreateInstance, StructuredStorage::PROPBAG2, CLSCTX_INPROC_SERVER, STREAM_SEEK_END,
            STREAM_SEEK_SET,
        },
        UI::Shell::SHCreateMemStream,
    },
};

pub(super) const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_PIXELS: u64 = 12_000_000;
const OUTPUT_LIMIT: u32 = 768;

fn dimensions_allowed(width: u32, height: u32) -> bool {
    width > 0 && height > 0 && u64::from(width) * u64::from(height) <= MAX_PIXELS
}

unsafe fn decode(factory: &IWICImagingFactory, raw: &[u8]) -> Option<IWICBitmapSource> {
    if raw.get(0..4) == Some(b"RIFF") && raw.get(8..12) == Some(b"WEBP") {
        let mut decoder = WebPDecoder::new(Cursor::new(raw)).ok()?;
        decoder.set_memory_limit(96 * 1024 * 1024);
        let (width, height) = decoder.dimensions();
        if !dimensions_allowed(width, height) {
            return None;
        }
        let (format, bytes) = if decoder.has_alpha() {
            (GUID_WICPixelFormat32bppRGBA, 4)
        } else {
            (GUID_WICPixelFormat24bppRGB, 3)
        };
        let mut pixels = vec![0; decoder.output_buffer_size()?];
        decoder.read_image(&mut pixels).ok()?;
        return factory
            .CreateBitmapFromMemory(width, height, &format, width * bytes, &pixels)
            .ok()?
            .cast()
            .ok();
    }
    let (container, canvas) = if raw.starts_with(&[0xff, 0xd8, 0xff]) {
        (GUID_ContainerFormatJpeg, None)
    } else if raw.starts_with(b"\x89PNG\r\n\x1a\n") {
        (GUID_ContainerFormatPng, None)
    } else if raw.starts_with(b"GIF87a") || raw.starts_with(b"GIF89a") {
        let width = u32::from(u16::from_le_bytes(raw.get(6..8)?.try_into().ok()?));
        let height = u32::from(u16::from_le_bytes(raw.get(8..10)?.try_into().ok()?));
        if !dimensions_allowed(width, height) {
            return None;
        }
        (GUID_ContainerFormatGif, Some((width, height)))
    } else {
        return None;
    };
    let stream = SHCreateMemStream(Some(raw))?;
    let decoder = factory.CreateDecoder(&container, null()).ok()?;
    decoder
        .Initialize(&stream, WICDecodeMetadataCacheOnDemand)
        .ok()?;
    let frame = decoder.GetFrame(0).ok()?;
    if let Some(canvas) = canvas {
        return gif_canvas(factory, raw, frame, canvas);
    }
    frame.cast().ok()
}

unsafe fn bgr(factory: &IWICImagingFactory, source: &IWICBitmapSource) -> Option<IWICBitmapSource> {
    let converter = factory.CreateFormatConverter().ok()?;
    converter
        .Initialize(
            source,
            &GUID_WICPixelFormat24bppBGR,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .ok()?;
    converter.cast().ok()
}

unsafe fn gif_canvas(
    factory: &IWICImagingFactory,
    raw: &[u8],
    frame: IWICBitmapFrameDecode,
    canvas: (u32, u32),
) -> Option<IWICBitmapSource> {
    let source: IWICBitmapSource = frame.cast().ok()?;
    let (mut width, mut height) = (0, 0);
    source.GetSize(&mut width, &mut height).ok()?;
    if (width, height) == canvas {
        return Some(source);
    }
    let metadata = frame.GetMetadataQueryReader().ok()?;
    let offset = |name: PCWSTR| {
        let mut value = PROPVARIANT::default();
        metadata.GetMetadataByName(name, &mut value).ok()?;
        u16::try_from(&value).ok().map(u32::from)
    };
    let left = offset(w!("/imgdesc/Left"))?;
    let top = offset(w!("/imgdesc/Top"))?;
    if !dimensions_allowed(width, height)
        || left.checked_add(width)? > canvas.0
        || top.checked_add(height)? > canvas.1
    {
        return None;
    }
    // The logical GIF screen can be larger than the first image rectangle.
    let mut background = [0; 3];
    let packed = *raw.get(10)?;
    if packed & 0x80 != 0 {
        let count = 2usize << (packed & 7);
        let index = usize::from(*raw.get(11)?);
        if index < count {
            if let Some(rgb) = raw.get(13 + 3 * index..16 + 3 * index) {
                background = [rgb[2], rgb[1], rgb[0]];
            }
        }
    }
    let stride = width as usize * 3;
    let canvas_stride = canvas.0 as usize * 3;
    let mut pixels = vec![0; stride * height as usize];
    bgr(factory, &source)?
        .CopyPixels(null(), stride as u32, &mut pixels)
        .ok()?;
    let mut full = vec![0; canvas_stride * canvas.1 as usize];
    for pixel in full.chunks_exact_mut(3) {
        pixel.copy_from_slice(&background);
    }
    let start = top as usize * canvas_stride + left as usize * 3;
    for (row, line) in pixels.chunks_exact(stride).enumerate() {
        let at = start + row * canvas_stride;
        full[at..at + stride].copy_from_slice(line);
    }
    factory
        .CreateBitmapFromMemory(
            canvas.0,
            canvas.1,
            &GUID_WICPixelFormat24bppBGR,
            canvas_stride as u32,
            &full,
        )
        .ok()?
        .cast()
        .ok()
}

unsafe fn jpeg(factory: &IWICImagingFactory, source: &IWICBitmapSource) -> Option<Vec<u8>> {
    let (mut width, mut height) = (0, 0);
    source.GetSize(&mut width, &mut height).ok()?;
    if !dimensions_allowed(width, height) {
        return None;
    }
    let mut source = bgr(factory, source)?;
    if width.max(height) > OUTPUT_LIMIT {
        let scale = f64::from(OUTPUT_LIMIT) / f64::from(width.max(height));
        width = (f64::from(width) * scale).round_ties_even().max(1.0) as u32;
        height = (f64::from(height) * scale).round_ties_even().max(1.0) as u32;
        let scaler = factory.CreateBitmapScaler().ok()?;
        scaler
            .Initialize(
                &source,
                width,
                height,
                WICBitmapInterpolationModeHighQualityCubic,
            )
            .ok()?;
        source = scaler.cast().ok()?;
    }
    let stride = width * 3;
    let mut pixels = vec![0; (stride * height) as usize];
    source.CopyPixels(null(), stride, &mut pixels).ok()?;

    let stream = SHCreateMemStream(None)?;
    let encoder = factory
        .CreateEncoder(&GUID_ContainerFormatJpeg, null())
        .ok()?;
    encoder.Initialize(&stream, WICBitmapEncoderNoCache).ok()?;
    let (mut frame, mut options) = (None, None);
    encoder.CreateNewFrame(&mut frame, &mut options).ok()?;
    let (frame, options) = (frame?, options?);
    let quality = PROPBAG2 {
        pstrName: PWSTR(w!("ImageQuality").0.cast_mut()),
        ..Default::default()
    };
    options.Write(1, &quality, &VARIANT::from(0.85_f32)).ok()?;
    frame.Initialize(&options).ok()?;
    frame.SetSize(width, height).ok()?;
    let mut format = GUID_WICPixelFormat24bppBGR;
    frame.SetPixelFormat(&mut format).ok()?;
    if format != GUID_WICPixelFormat24bppBGR {
        return None;
    }
    // A fresh frame receives pixels only, so source metadata and frames cannot be carried over.
    frame.WritePixels(height, stride, &pixels).ok()?;
    frame.Commit().ok()?;
    encoder.Commit().ok()?;
    let mut length = 0;
    stream.Seek(0, STREAM_SEEK_END, Some(&mut length)).ok()?;
    if length == 0 || length > MAX_BYTES as u64 {
        return None;
    }
    stream.Seek(0, STREAM_SEEK_SET, None).ok()?;
    let mut jpeg = vec![0; length as usize];
    let mut read = 0;
    stream
        .Read(jpeg.as_mut_ptr().cast(), length as u32, Some(&mut read))
        .ok()
        .ok()?;
    (u64::from(read) == length).then_some(jpeg)
}

pub(super) fn encode(raw: &[u8]) -> Option<String> {
    let _apartment = match super::Apartment::enter() {
        Ok(apartment) => Some(apartment),
        // WIC also works in an STA that the caller already initialized.
        Err(error) if error.code() == RPC_E_CHANGED_MODE => None,
        Err(_) => return None,
    };
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let image = decode(&factory, raw)?;
        let jpeg = jpeg(&factory, &image)?;
        Some(format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg)))
    }
}

pub(super) fn item_url(value: &str) -> Option<String> {
    let path = value
        .strip_prefix("https://xpui.app.spotify.com/")
        .or_else(|| value.strip_prefix("https://open.spotify.com/"))?;
    let path = match path.split_once('?') {
        Some((path, query)) if !query.contains(|ch: char| ch == '#' || ch.is_whitespace()) => path,
        Some(_) => return None,
        None => path,
    };
    let (kind, id) = path.split_once('/')?;
    (matches!(kind, "album" | "track" | "episode")
        && id.len() == 22
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric()))
    .then(|| format!("https://open.spotify.com/{path}"))
}

fn thumbnail_url(value: &str) -> Option<&str> {
    let (host, image) = value.strip_prefix("https://")?.split_once("/image/")?;
    let cdn_host = host
        .strip_prefix("image-cdn-")
        .and_then(|host| host.strip_suffix(".spotifycdn.com"));
    let approved = host == "i.scdn.co"
        || cdn_host.is_some_and(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')
        });
    (approved && !image.is_empty() && image.bytes().all(|ch| ch.is_ascii_alphanumeric()))
        .then_some(value)
}

struct Entry {
    item: String,
    retry_at: f64,
    cover: Value,
}

#[derive(Default)]
struct ArtworkCache(Mutex<Option<Entry>>);

impl ArtworkCache {
    fn lookup(&self, item: &str, now: f64) -> Value {
        let Ok(mut cache) = self.0.lock() else {
            return json!({"cached":true,"cover":null});
        };
        if let Some(entry) = cache.as_ref() {
            if entry.item == item && (!entry.cover.is_null() || now < entry.retry_at) {
                return json!({"cached":true,"cover":entry.cover});
            }
        }
        *cache = Some(Entry {
            item: item.to_owned(),
            retry_at: now + 20.0,
            cover: Value::Null,
        });
        json!({"cached":false})
    }

    fn store(&self, item: &str, cover: &Value) {
        if let Ok(mut cache) = self.0.lock() {
            if let Some(entry) = cache.as_mut().filter(|entry| entry.item == item) {
                entry.cover = cover.clone();
            }
        }
    }
}

fn reply(value: Value) -> *mut c_char {
    super::encode_response(Ok(value))
}

#[no_mangle]
unsafe extern "C" fn lummix_media_encode_artwork(data: *const u8, len: usize) -> *mut c_char {
    if data.is_null() || len > MAX_BYTES {
        return reply(Value::Null);
    }
    reply(json!(encode(unsafe {
        std::slice::from_raw_parts(data, len)
    })))
}

#[no_mangle]
unsafe extern "C" fn lummix_media_artwork_url(kind: u32, url: *const c_char) -> *mut c_char {
    if url.is_null() {
        return reply(Value::Null);
    }
    let Ok(url) = (unsafe { CStr::from_ptr(url) }).to_str() else {
        return reply(Value::Null);
    };
    reply(match kind {
        0 => json!(item_url(url)),
        1 => json!(thumbnail_url(url)),
        _ => Value::Null,
    })
}

#[no_mangle]
extern "C" fn lummix_media_artwork_cache_new() -> *mut ArtworkCache {
    Box::into_raw(Box::new(ArtworkCache::default()))
}

#[no_mangle]
unsafe extern "C" fn lummix_media_artwork_cache_free(cache: *mut ArtworkCache) {
    if !cache.is_null() {
        drop(unsafe { Box::from_raw(cache) });
    }
}

#[no_mangle]
unsafe extern "C" fn lummix_media_artwork_cache_call(
    cache: *const ArtworkCache,
    op: u32,
    item: *const c_char,
    request: *const c_char,
    now: f64,
) -> *mut c_char {
    if cache.is_null() || item.is_null() || !now.is_finite() {
        return ptr::null_mut();
    }
    let Ok(item) = (unsafe { CStr::from_ptr(item) }).to_str() else {
        return ptr::null_mut();
    };
    let cache = unsafe { &*cache };
    if op == 0 {
        return reply(cache.lookup(item, now));
    }
    if op == 1 && !request.is_null() {
        if let Ok(cover) =
            serde_json::from_slice::<Value>(unsafe { CStr::from_ptr(request).to_bytes() })
        {
            cache.store(item, &cover);
        }
    }
    reply(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_cdn_and_rate_limits_cover_failures() {
        assert!(thumbnail_url("https://image-cdn-abc-1.spotifycdn.com/image/C0ver").is_some());
        for bad in [
            "https://i.scdn.co:443/image/abc",
            "https://i.scdn.co/image/abc?x",
            "https://image-cdn-.spotifycdn.com/image/abc",
        ] {
            assert!(thumbnail_url(bad).is_none());
        }
        let cache = ArtworkCache::default();
        assert_eq!(cache.lookup("album", 5.0), json!({"cached":false}));
        assert_eq!(
            cache.lookup("album", 24.0),
            json!({"cached":true,"cover":null})
        );
        assert_eq!(cache.lookup("album", 25.0), json!({"cached":false}));
        cache.store("album", &json!(["hash", "cover"]));
        assert_eq!(
            cache.lookup("album", 50.0),
            json!({"cached":true,"cover":["hash","cover"]})
        );
        assert_eq!(cache.lookup("track", 50.0), json!({"cached":false}));
    }
}
