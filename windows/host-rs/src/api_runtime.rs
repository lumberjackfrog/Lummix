use crate::{diagnostics, media_http, mixer_api, profile};
use lummix_media::api::{PlayerApi, CAPABILITIES, ROUTES};
use serde_json::{json, Map, Value};
use std::{
    env,
    ffi::OsString,
    io,
    path::{Component, Path, PathBuf, Prefix},
    sync::{mpsc, Arc},
};

fn locations(data: &Map<String, Value>) -> bool {
    data.iter().all(|(k,v)|matches!(k.as_str(),"desktop"|"store") && v.as_str().is_some_and(|s|{
        let p=Path::new(s);
        matches!(p.components().next(),Some(Component::Prefix(q)) if matches!(q.kind(),Prefix::Disk(_)))
            && p.is_absolute() && !p.components().any(|q|q==Component::ParentDir)
            && p.file_name().and_then(|n|n.to_str()).is_some_and(|n|n.eq_ignore_ascii_case("spotify.exe"))
    }))
}
fn health(supported: bool) -> Value {
    let mut caps = CAPABILITIES.to_vec();
    if supported {
        caps.push("spotify-process-mix-v1");
    }
    caps.extend([
        "mixer-duck-v1",
        "mic-route-v1",
        "mix-setup-alias-v1",
        "cable-restart-v1",
        "studio-preferences-v1",
    ]);
    json!({"service":"lummix-media","protocol":2,"capabilities":caps,"runtime":"rust"})
}
fn capture_check(supported: bool) -> Value {
    let mut result =
        json!({"supported":supported,"playerFound":false,"attached":false,"signal":false});
    match lummix_media::api::diagnostic_target() {
        Ok(t) if t["state"] == 1 => {
            result["playerFound"] = json!(true);
            result["edition"] = t["edition"].clone();
            if supported {
                let outcome = t["pid"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .zip(t["created"].as_f64())
                    .ok_or("unavailable")
                    .and_then(|(p, c)| lummix_dsp::runtime::check_capture(p, c));
                match outcome {
                    Ok(s) => {
                        result["attached"] = json!(matches!(s, 1 | 2));
                        result["signal"] = json!(s == 2);
                        result["error"] = if s < 0 { json!(s) } else { Value::Null };
                    }
                    Err(e) => result["error"] = json!(e),
                }
            }
        }
        Ok(t) if t["state"] == 2 => result["error"] = json!("ambiguous"),
        Err(_) => result["error"] = json!("unavailable"),
        Ok(t) if t["state"] == 4 => result["error"] = json!("unavailable"),
        _ => {}
    }
    result
}
pub(crate) fn run_mode(mode: &str, args: &[OsString], assets: &Path) -> Result<bool, String> {
    if mode == "--runtime-info" && args.is_empty() {
        println!(
            "{}",
            json!({"runtime":"rust","python":false,"protocol":2,"components":["host","dsp","media","controls-wasm"]})
        );
        return Ok(true);
    }
    if mode == "verify-audio-driver" && args.is_empty() {
        lummix_dsp::runtime::verify_driver(&assets.join("libportaudio64bit.dll"))?;
        println!(
            "{}",
            json!({"runtime":"rust","driver":"portaudio","cycles":2})
        );
        return Ok(true);
    }
    if !matches!(mode, "media" | "spotify" | "check-spotify") {
        return Ok(false);
    }
    let supported = diagnostics::windows_build() >= 20348;
    if mode == "check-spotify" {
        if !args.is_empty() {
            return Err("Unexpected arguments for check-spotify".into());
        }
        println!("{}", capture_check(supported));
        return Ok(true);
    }
    if args.iter().any(|a| a != "--parent-stdin") {
        return Err("Unexpected arguments for media".into());
    }
    let data = PathBuf::from(env::var_os("LOCALAPPDATA").ok_or("Missing Windows local app data")?)
        .join("LumMix");
    std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
    let read_path = data.join("spotify-locations.json");
    let write_path = read_path.clone();
    let player = Arc::new(PlayerApi::with_profile_hooks(
        &data,
        move || {
            profile::load_profile(&read_path, locations)
                .ok()
                .and_then(|p| p.get("desktop")?.as_str().map(PathBuf::from))
        },
        move |edition, path| {
            if let Some(s) = path.to_str() {
                let _ = profile::update_profile(
                    &write_path,
                    Map::from_iter([(edition.to_owned(), json!(s))]),
                    locations,
                );
            }
        },
    )?);
    let mixer = Arc::new(mixer_api::Mixer::new(&data, assets, supported));
    let mut routes = ROUTES.to_vec();
    routes.extend(mixer_api::ROUTES);
    routes.push(("GET", "/lummix-health"));
    let health = health(supported).to_string().into_bytes();
    let (p, m) = (Arc::clone(&player), Arc::clone(&mixer));
    let qa = env::var_os("LUMMIX_QA_LOOPBACK").is_some_and(|q| q == "2");
    let listener = media_http::listen_native(
        &routes,
        qa,
        Box::new(move |method, path, origin, query, headers, body| {
            if path == "/lummix-health" {
                (200, health.clone())
            } else if path.starts_with("/spotify/") {
                p.handle(method, path, origin, query, headers, body.bytes)
            } else {
                let (status, body) = m.handle(method, path, origin, headers, body);
                (status, mixer_api::response_body(status, body))
            }
        }),
    )
    .ok_or("Could not start Lummix media on the loopback address")?;
    let (stop, wait) = mpsc::channel();
    let monitor = mixer.watch(wait);
    if args.iter().any(|a| a == "--parent-stdin") {
        io::copy(&mut io::stdin().lock(), &mut io::sink()).map_err(|e| e.to_string())?;
    } else {
        loop {
            std::thread::park();
        }
    }
    player.shutdown();
    drop(listener);
    drop(stop);
    let _ = monitor.join();
    mixer.shutdown();
    Ok(true)
}
