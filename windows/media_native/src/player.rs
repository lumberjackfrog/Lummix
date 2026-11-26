use super::{
    artwork, control, processes, retain_session, selected_target, session, source, store_paths,
    timeline, window_player, PlaybackStatus, Session, STORE_PLAYER,
};
use serde_json::{json, Value};
use std::{
    io::ErrorKind,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use windows::{
    core::{Result, PCWSTR},
    Win32::Security::Cryptography::{
        BCryptCloseAlgorithmProvider, BCryptHash, BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE,
        BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
    },
};

struct Inventory {
    at: Instant,
    revision: u64,
    fields: Value,
}

fn inventory(preferred: Option<&Path>) -> Value {
    static CACHE: OnceLock<Mutex<Option<Inventory>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Mutex::default).lock().unwrap();
    if let Some(inventory) = cache
        .as_ref()
        .filter(|inventory| inventory.at.elapsed() < Duration::from_secs(30))
    {
        return inventory.fields.clone();
    }
    let revision = cache.as_ref().map_or(1, |inventory| inventory.revision + 1);
    let desktop = source::desktop_executable(preferred);
    let store = store_paths();
    let mut editions = Vec::new();
    if desktop.as_ref().is_ok_and(|path| path.is_some()) {
        editions.push("desktop");
    }
    if store.as_ref().is_ok_and(|paths| !paths.is_empty()) {
        editions.push("store");
    }
    let store_path = store.as_ref().ok().and_then(|paths| {
        let mut denied = None;
        for root in paths.iter().flatten() {
            let path = Path::new(root).join("Spotify.exe");
            if source::normalize(&path).is_none() {
                continue;
            }
            match path.metadata() {
                Ok(metadata) if metadata.is_file() => return Some(path),
                Err(error) if error.kind() == ErrorKind::PermissionDenied && denied.is_none() => {
                    denied = Some(path);
                }
                _ => {}
            }
        }
        denied
    });
    let fields = json!({
        "installations":editions,"installationKnown":desktop.is_ok() && store.is_ok(),
        "inventoryRevision":revision,
        "inventoryPaths":{"desktop":desktop.ok().flatten(),"store":store_path},
    });
    *cache = Some(Inventory {
        at: Instant::now(),
        revision,
        fields: fields.clone(),
    });
    fields
}

#[derive(Default)]
struct Cover {
    key: Option<(String, (String, String, String, i32), [u8; 32])>,
    value: Value,
}

fn cover_cache() -> &'static Mutex<Cover> {
    static CACHE: OnceLock<Mutex<Cover>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn fingerprint(raw: &[u8]) -> Option<[u8; 32]> {
    let mut algorithm = BCRYPT_ALG_HANDLE::default();
    let mut digest = [0; 32];
    unsafe {
        BCryptOpenAlgorithmProvider(
            &mut algorithm,
            BCRYPT_SHA256_ALGORITHM,
            PCWSTR::null(),
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
        .ok()
        .ok()?;
        let result = BCryptHash(algorithm, None, raw, &mut digest).ok();
        let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
        result.ok()?;
    }
    Some(digest)
}

fn current_cover(
    edition: &str,
    labels: (String, String, String, i32),
    raw: Option<Vec<u8>>,
) -> Value {
    let mut cache = cover_cache().lock().unwrap();
    let Some(raw) = raw else {
        *cache = Cover::default();
        return Value::Null;
    };
    let Some(digest) = fingerprint(&raw) else {
        return Value::Null;
    };
    let key = (edition.to_owned(), labels, digest);
    if cache.key.as_ref() != Some(&key) {
        cache.value = artwork::encode(&raw).map_or(Value::Null, |artwork| {
            let id = digest[..12]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            json!({"artwork":artwork,"artworkId":id})
        });
        cache.key = Some(key);
    }
    cache.value.clone()
}

fn clip_text(value: &str) -> String {
    value.chars().take(256).collect()
}

fn window_snapshot() -> Result<Option<Value>> {
    let processes = source::verified_processes()?;
    let mut roots = processes.iter().filter(|(process, _)| {
        !processes
            .iter()
            .any(|(parent, _)| parent.pid == process.parent)
    });
    let Some((process, edition)) = roots.next() else {
        return Ok(None);
    };
    if roots.next().is_some() {
        return Ok(Some(json!({"state":"ambiguous"})));
    }
    let edition = if *edition == 1 { "desktop" } else { "store" };
    let mut result = json!({"state":"detected","edition":edition});
    let Ok(windows) = window_player::visible_windows() else {
        return Ok(Some(result));
    };
    let matches = windows
        .as_array()
        .into_iter()
        .flatten()
        .filter(|window| window[1].as_u64() == Some(u64::from(process.pid)))
        .filter_map(|window| window[0].as_u64())
        .collect::<Vec<_>>();
    let [handle] = matches.as_slice() else {
        return Ok(Some(result));
    };
    if !processes::matches_creation(process.pid, process.created) {
        return Ok(Some(result));
    }
    let title = window_player::window_title(*handle, process.pid);
    if let Some((artist, title)) = title.as_str().and_then(|title| title.split_once(" - ")) {
        if !artist.trim().is_empty() && !title.trim().is_empty() {
            result["artist"] = json!(clip_text(artist.trim()));
            result["title"] = json!(clip_text(title.trim()));
        }
    }
    if let Ok(mut transport) = window_player::transport(*handle, process.pid, 0) {
        if transport["reason"].is_null() {
            let ready = transport["canPlay"] == true || transport["canPause"] == true;
            if ready {
                result["state"] = json!("ready");
                result["transportSource"] = json!("spotify_window");
                result["window"] = json!([
                    edition,
                    [json!(handle), json!(process.pid), json!(process.created)]
                ]);
            }
            result
                .as_object_mut()
                .unwrap()
                .append(transport.as_object_mut().unwrap());
        }
    }
    Ok(Some(result))
}

pub(super) fn snapshot(preferred: Option<&Path>) -> Result<Value> {
    let mut state = match selected_target() {
        Ok(Ok((session, source))) => {
            let edition = edition(&source);
            native_snapshot(session, source)
                .unwrap_or_else(|_| json!({"state":"unavailable","edition":edition}))
        }
        selection => {
            *cover_cache().lock().unwrap() = Cover::default();
            let state = match selection {
                Ok(Err(state)) => state,
                _ => "unavailable",
            };
            if state == "ambiguous" {
                json!({"state":state})
            } else {
                window_snapshot()
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| json!({"state":state}))
            }
        }
    };
    state
        .as_object_mut()
        .unwrap()
        .append(inventory(preferred).as_object_mut().unwrap());
    Ok(state)
}

fn edition(source: &str) -> &'static str {
    if source.eq_ignore_ascii_case(STORE_PLAYER) {
        "store"
    } else {
        "desktop"
    }
}

fn native_snapshot(session: Session, source: String) -> Result<Value> {
    let edition = edition(&source);
    let Ok(info) = session.GetPlaybackInfo() else {
        return Ok(json!({"state":"unavailable","edition":edition}));
    };
    let Ok(controls) = info.Controls() else {
        return Ok(json!({"state":"unavailable","edition":edition}));
    };
    let timeline = timeline(&session).unwrap_or(Value::Null);
    let mut result = json!({
        "state":"ready","edition":edition,"playing":info.PlaybackStatus()? == PlaybackStatus::Playing,
        "canPlay":controls.IsPlayEnabled()?,"canPause":controls.IsPauseEnabled()?,
        "canNext":controls.IsNextEnabled()?,"canPrevious":controls.IsPreviousEnabled()?,
        "canStop":controls.IsStopEnabled()?,
        "canSeek":controls.IsPlaybackPositionEnabled()? && !timeline["minSeek"].is_null(),
        "timeline":timeline,"itemUrl":null,
    });
    if let Ok(properties) = super::media_properties(&session) {
        let title = properties.Title().unwrap_or_default().to_string();
        let artist = properties.Artist().unwrap_or_default().to_string();
        let album = properties.AlbumTitle().unwrap_or_default().to_string();
        let track = properties.TrackNumber().unwrap_or_default();
        result["title"] = json!(clip_text(&title));
        result["artist"] = json!(clip_text(&artist));
        let thumbnail = current_cover(
            edition,
            (title, artist, album, track),
            super::artwork(&properties).ok().flatten(),
        );
        if let Some(artwork) = thumbnail.as_object() {
            result.as_object_mut().unwrap().extend(artwork.clone());
        } else if let Some(window) = window_snapshot().ok().flatten() {
            let same = |field: &str| {
                !result[field].as_str().unwrap_or_default().is_empty()
                    && result[field].as_str().unwrap_or_default().to_lowercase()
                        == window[field].as_str().unwrap_or_default().to_lowercase()
            };
            if window["edition"] == edition && same("title") && same("artist") {
                result["itemUrl"] = window["itemUrl"].clone();
            }
        }
    }
    result["session"] = retain_session((session, source));
    Ok(result)
}

fn seek_target(timeline: &Value, direction: i32) -> Option<i64> {
    let min_seek = timeline["minSeek"].as_f64()?;
    let max_seek = timeline["maxSeek"].as_f64()?;
    let position = timeline["position"].as_f64()?;
    Some(
        ((position + f64::from(direction * 15)).clamp(min_seek, max_seek) * 10_000_000.0)
            .round_ties_even() as i64,
    )
}

fn failure(state: &str, error: &str, status: u32, refresh: bool) -> Value {
    json!({"state":state,"error":error,"status":status,"refresh":refresh})
}

fn already_satisfied(status: PlaybackStatus, action: u32) -> bool {
    (action == 0 && status == PlaybackStatus::Playing)
        || (action == 1 && status == PlaybackStatus::Paused)
}

pub(super) fn command(id: u64, action: u32) -> Result<Value> {
    let original = session(id)?;
    let current = match selected_target() {
        Ok(Ok((session, _))) if session == original => session,
        Ok(Err(state)) => return Ok(failure(state, "selection_changed", 409, false)),
        _ => return Ok(failure("unavailable", "selection_changed", 409, false)),
    };
    let original_info = original.GetPlaybackInfo()?;
    let status = original_info.PlaybackStatus()?;
    if current.GetPlaybackInfo()?.PlaybackStatus()? != status {
        return Ok(failure("unavailable", "selection_changed", 409, false));
    }
    if already_satisfied(status, action) {
        return Ok(json!({"status":200,"refresh":true}));
    }
    let controls = original_info.Controls()?;
    let (enabled, position) = match action {
        0 => (controls.IsPlayEnabled()?, None),
        1 => (controls.IsPauseEnabled()?, None),
        2 => (controls.IsPreviousEnabled()?, None),
        3 => (controls.IsNextEnabled()?, None),
        4 => (controls.IsStopEnabled()?, None),
        5 | 6 => {
            let target = seek_target(
                &timeline(&original).unwrap_or(Value::Null),
                if action == 5 { -1 } else { 1 },
            );
            (
                controls.IsPlaybackPositionEnabled()? && target.is_some(),
                target,
            )
        }
        _ => (false, None),
    };
    if !enabled {
        return Ok(failure("ready", "control_disabled", 409, true));
    }
    let accepted = control(
        &original,
        if action == 6 { 5 } else { action },
        position.unwrap_or_default(),
    )?;
    if accepted == true {
        Ok(json!({"status":200,"refresh":true}))
    } else {
        Ok(failure("ready", "player_declined", 409, true))
    }
}

pub(super) fn selection_matches(id: u64) -> Result<Value> {
    let original = session(id)?;
    let matches = matches!(selected_target()?, Ok((selected, _)) if selected == original);
    Ok(json!(matches))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_seek_is_clamped_to_the_published_interval() {
        let timeline = json!({"minSeek":2.0,"maxSeek":120.0,"position":110.0});
        assert_eq!(seek_target(&timeline, 1), Some(1_200_000_000));
        assert_eq!(seek_target(&timeline, -1), Some(950_000_000));
        assert_eq!(seek_target(&Value::Null, 1), None);
    }

    #[test]
    fn only_matching_play_or_pause_is_idempotent() {
        assert!(already_satisfied(PlaybackStatus::Playing, 0));
        assert!(already_satisfied(PlaybackStatus::Paused, 1));
        assert!(!already_satisfied(PlaybackStatus::Paused, 0));
        assert!(!already_satisfied(PlaybackStatus::Playing, 1));
        assert!(!already_satisfied(PlaybackStatus::Playing, 3));
    }
}
