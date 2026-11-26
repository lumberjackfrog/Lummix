use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    ffi::{c_char, CStr, CString},
    sync::{Mutex, MutexGuard, OnceLock},
    thread::sleep,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{
    core::{Error, Result, HSTRING},
    Foundation::{
        AsyncStatus, Collections::IVectorView, EventRegistrationToken, TypedEventHandler,
    },
    Management::Deployment::{PackageManager, PackageTypes},
    Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    },
    Storage::Streams::DataReader,
    Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
};

mod artwork;
mod audio_sessions;
mod capability;
mod change_feed;
mod player;
mod processes;
mod registry;
mod setup;
mod source;
mod volume;
mod window_player;

const DESKTOP: &str = "spotify.exe";
const STORE_FAMILY: &str = "SpotifyAB.SpotifyMusic_zpdnekdrzrea0";
const STORE_PLAYER: &str = "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify";

fn spotify_source(source: &str) -> bool {
    source.eq_ignore_ascii_case(DESKTOP) || source.eq_ignore_ascii_case(STORE_PLAYER)
}

struct Apartment;
impl Apartment {
    fn enter() -> Result<Self> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

// WinRT Future completion may stall while a player starts or goes away.
macro_rules! wait_for {
    ($operation:expr, $milliseconds:expr) => {{
        let operation = $operation?;
        let deadline = Instant::now() + Duration::from_millis($milliseconds);
        loop {
            if operation.Status()? != AsyncStatus::Started {
                break operation.GetResults();
            }
            if Instant::now() >= deadline {
                let _ = operation.Cancel();
                break Err(Error::from_hresult(windows::core::HRESULT(
                    0x800705b4u32 as i32,
                )));
            }
            sleep(Duration::from_millis(10));
        }
    }};
}

#[derive(Default)]
struct Media {
    manager: Option<Manager>,
    next_id: u64,
    sessions: HashMap<u64, Session>,
    manager_events: HashMap<u64, (Manager, EventRegistrationToken)>,
}

fn media() -> MutexGuard<'static, Media> {
    static MEDIA: OnceLock<Mutex<Media>> = OnceLock::new();
    MEDIA.get_or_init(Mutex::default).lock().unwrap()
}

fn manager() -> Result<Manager> {
    let mut state = media();
    if let Some(manager) = &state.manager {
        return Ok(manager.clone());
    }
    let manager = wait_for!(Manager::RequestAsync(), 2400)?;
    state.manager = Some(manager.clone());
    Ok(manager)
}

fn session(id: u64) -> Result<Session> {
    media()
        .sessions
        .get(&id)
        .cloned()
        .ok_or_else(|| Error::from_hresult(windows::core::HRESULT(0x80070006u32 as i32)))
}

fn current_sessions() -> Result<IVectorView<Session>> {
    let manager = manager()?;
    manager
        .GetSessions()
        .inspect_err(|_| discard_manager(&manager))
}

fn discard_manager(manager: &Manager) {
    let mut state = media();
    if state.manager.as_ref() == Some(manager) {
        state.manager = None;
    }
}

fn listed_spotify() -> Result<Vec<(Session, String)>> {
    let sessions = current_sessions()?;
    let mut selected = Vec::new();
    for index in 0..sessions.Size()? {
        let session = sessions.GetAt(index)?;
        let source = session.SourceAppUserModelId()?.to_string();
        if spotify_source(&source) {
            selected.push((session, source));
        }
    }
    Ok(selected)
}

fn retain_session((session, source): (Session, String)) -> Value {
    let mut state = media();
    state.next_id += 1;
    let id = state.next_id;
    state.sessions.insert(id, session);
    json!([id, source])
}

fn sessions() -> Result<Value> {
    Ok(json!(listed_spotify()?
        .into_iter()
        .map(retain_session)
        .collect::<Vec<_>>()))
}

fn choose_session<T>(
    choices: Vec<T>,
    mut is_playing: impl FnMut(&T) -> Result<bool>,
) -> std::result::Result<T, &'static str> {
    match choices.len() {
        0 => Err("no_session"),
        1 => Ok(choices.into_iter().next().unwrap()),
        _ => {
            let mut selected = None;
            for choice in choices {
                match is_playing(&choice) {
                    Ok(false) => {}
                    Ok(true) if selected.is_none() => selected = Some(choice),
                    _ => return Err("ambiguous"),
                }
            }
            selected.ok_or("ambiguous")
        }
    }
}

fn selected_session() -> Result<Value> {
    Ok(match selected_target()? {
        Ok(session) => json!({"state": "ready", "session": retain_session(session)}),
        Err(state) => json!({"state": state}),
    })
}

fn selected_target() -> Result<std::result::Result<(Session, String), &'static str>> {
    Ok(choose_session(listed_spotify()?, |(session, _)| {
        Ok(session.GetPlaybackInfo()?.PlaybackStatus()? == PlaybackStatus::Playing)
    }))
}

fn timeline_values(
    span: (f64, f64),
    seeks: (f64, f64),
    mut position: f64,
    rate: f64,
    playing: bool,
    updated: Option<f64>,
    now: f64,
) -> Value {
    let (start, end) = span;
    if end <= start {
        return Value::Null;
    }
    let rate = if rate.is_finite() && rate > 0.0 {
        rate
    } else {
        1.0
    };
    if playing {
        if let Some(updated) = updated {
            position += (now - updated).max(0.0) * rate;
        }
    }
    let min_seek = start.max(seeks.0);
    let max_seek = end.min(seeks.1);
    let seekable = max_seek > min_seek;
    json!({
        "start": start,
        "end": end,
        "position": position.clamp(start, end),
        "rate": rate,
        "sampledAt": now,
        "minSeek": seekable.then_some(min_seek),
        "maxSeek": seekable.then_some(max_seek),
    })
}

fn playback(session: &Session) -> Result<Value> {
    let info = session.GetPlaybackInfo()?;
    let controls = info.Controls()?;
    let rate = info.PlaybackRate().and_then(|rate| rate.Value()).ok();
    Ok(json!({
        "status": info.PlaybackStatus()?.0,
        "playback_rate": rate,
        "controls": {
            "is_play_enabled": controls.IsPlayEnabled()?,
            "is_pause_enabled": controls.IsPauseEnabled()?,
            "is_previous_enabled": controls.IsPreviousEnabled()?,
            "is_next_enabled": controls.IsNextEnabled()?,
            "is_stop_enabled": controls.IsStopEnabled()?,
            "is_playback_position_enabled": controls.IsPlaybackPositionEnabled()?,
        },
    }))
}

fn timeline(session: &Session) -> Result<Value> {
    let info = session.GetPlaybackInfo()?;
    let timeline = session.GetTimelineProperties()?;
    let seconds = |ticks: i64| ticks as f64 / 10_000_000.0;
    let updated = timeline.LastUpdatedTime()?.UniversalTime;
    let unix_epoch = 116_444_736_000_000_000;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    Ok(timeline_values(
        (
            seconds(timeline.StartTime()?.Duration),
            seconds(timeline.EndTime()?.Duration),
        ),
        (
            seconds(timeline.MinSeekTime()?.Duration),
            seconds(timeline.MaxSeekTime()?.Duration),
        ),
        seconds(timeline.Position()?.Duration),
        info.PlaybackRate()
            .and_then(|rate| rate.Value())
            .unwrap_or(1.0),
        info.PlaybackStatus()? == PlaybackStatus::Playing,
        (updated >= unix_epoch).then(|| seconds(updated - unix_epoch)),
        now,
    ))
}

fn artwork(
    properties: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties,
) -> Result<Option<Vec<u8>>> {
    let thumbnail = properties.Thumbnail()?;
    let stream = wait_for!(thumbnail.OpenReadAsync(), 680)?;
    let size = stream.Size()?;
    if size == 0 || size > artwork::MAX_BYTES as u64 {
        return Ok(None);
    }
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
    if wait_for!(reader.LoadAsync(size as u32), 680)? != size as u32 {
        return Ok(None);
    }
    let mut pixels = vec![0; size as usize];
    reader.ReadBytes(&mut pixels)?;
    Ok(Some(pixels))
}

fn now_playing(session: &Session) -> Result<Value> {
    let properties = media_properties(session)?;
    Ok(json!({
        "labels": [properties.Title()?.to_string(), properties.Artist()?.to_string(),
                   properties.AlbumTitle()?.to_string(), properties.TrackNumber()?],
        "thumbnail": artwork(&properties).ok().flatten().map(|raw| STANDARD.encode(raw)),
    }))
}

fn media_properties(
    session: &Session,
) -> Result<windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties> {
    wait_for!(session.TryGetMediaPropertiesAsync(), 1950)
}

fn control(session: &Session, action: u32, position: i64) -> Result<Value> {
    let current_source = session.SourceAppUserModelId()?.to_string();
    if !spotify_source(&current_source) {
        return Ok(json!(false));
    }
    let sessions = current_sessions()?;
    let mut still_available = false;
    for index in 0..sessions.Size()? {
        if &sessions.GetAt(index)? == session {
            still_available = true;
            break;
        }
    }
    if !still_available {
        return Ok(json!(false));
    }
    let controls = session.GetPlaybackInfo()?.Controls()?;
    let operation = match action {
        0 if controls.IsPlayEnabled()? => session.TryPlayAsync(),
        1 if controls.IsPauseEnabled()? => session.TryPauseAsync(),
        2 if controls.IsPreviousEnabled()? => session.TrySkipPreviousAsync(),
        3 if controls.IsNextEnabled()? => session.TrySkipNextAsync(),
        4 if controls.IsStopEnabled()? => session.TryStopAsync(),
        5 if controls.IsPlaybackPositionEnabled()? => {
            session.TryChangePlaybackPositionAsync(position)
        }
        _ => return Ok(json!(false)),
    };
    Ok(json!(wait_for!(operation, 2600)?))
}

fn store_paths() -> Result<Vec<Option<String>>> {
    let packages = PackageManager::new()?
        .FindPackagesByUserSecurityIdPackageFamilyNameWithPackageTypes(
            &HSTRING::new(),
            &HSTRING::from(STORE_FAMILY),
            PackageTypes::Main,
        )?;
    let mut paths = Vec::new();
    for package in packages {
        let path = package
            .InstalledPath()
            .ok()
            .filter(|path| !path.is_empty())
            .or_else(|| {
                package
                    .InstalledLocation()
                    .and_then(|folder| folder.Path())
                    .ok()
            });
        paths.push(path.map(|path| path.to_string()));
    }
    Ok(paths)
}

type MediaEvent = extern "C" fn(u64);

fn add_event(id: u64, kind: u32, callback: MediaEvent, event_id: u64) -> Result<Value> {
    let token = match kind {
        0 => {
            let manager = manager()?;
            let token = manager.SessionsChanged(&TypedEventHandler::new(move |_, _| {
                callback(event_id);
                Ok(())
            }))?;
            let mut state = media();
            state.next_id += 1;
            let id = state.next_id;
            state.manager_events.insert(id, (manager, token));
            return Ok(json!(id));
        }
        1 => session(id)?.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
            callback(event_id);
            Ok(())
        }))?,
        2 => session(id)?.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
            callback(event_id);
            Ok(())
        }))?,
        _ => {
            return Err(Error::from_hresult(windows::core::HRESULT(
                0x80070057u32 as i32,
            )))
        }
    };
    Ok(json!(token.Value))
}

fn remove_event(id: u64, kind: u32, token: i64) -> Result<Value> {
    if kind == 0 {
        let binding = media().manager_events.remove(&(token as u64));
        if let Some((manager, token)) = binding {
            manager.RemoveSessionsChanged(token)?;
        }
        return Ok(Value::Null);
    }
    let token = EventRegistrationToken { Value: token };
    match kind {
        1 => session(id)?.RemoveMediaPropertiesChanged(token)?,
        2 => session(id)?.RemovePlaybackInfoChanged(token)?,
        _ => {
            return Err(Error::from_hresult(windows::core::HRESULT(
                0x80070057u32 as i32,
            )))
        }
    }
    Ok(Value::Null)
}

fn response(read: impl FnOnce() -> Result<Value>) -> *mut c_char {
    encode_response(Apartment::enter().and_then(|_apartment| read()))
}

fn encode_response(result: Result<Value>) -> *mut c_char {
    let value = match result {
        Ok(value) => json!({ "value": value }),
        Err(error) => json!({ "error": format!("{error}") }),
    };
    CString::new(value.to_string()).unwrap().into_raw()
}

#[no_mangle]
pub extern "C" fn lummix_media_abi() -> u32 {
    1
}

#[no_mangle]
pub extern "C" fn lummix_media_call(op: u32, id: u64, action: u32, arg: i64) -> *mut c_char {
    response(|| match op {
        0 => sessions(),
        1 => playback(&session(id)?),
        2 => timeline(&session(id)?),
        3 => now_playing(&session(id)?),
        4 => control(&session(id)?, action, arg),
        5 => store_paths().map(|paths| json!(paths)),
        6 => manager().map(|_| Value::Null),
        7 => remove_event(id, action, arg),
        8 => registry::desktop_registrations().map(|values| json!(values)),
        9 => window_player::transport(id, arg as u32, action),
        10 => source::verified_processes().map(|rows| {
            json!(rows
                .into_iter()
                .map(|(row, edition)| {
                    (
                        row.pid,
                        row.parent,
                        row.created,
                        if edition == 1 { "desktop" } else { "store" },
                    )
                })
                .collect::<Vec<_>>())
        }),
        11 => Ok(json!(session(id)? == session(arg as u64)?)),
        12 => selected_session(),
        13 => window_player::visible_windows(),
        14 => Ok(window_player::window_title(id, action)),
        21 => player::command(id, action),
        22 => player::selection_matches(id),
        15 => source::diagnostic_target(),
        _ => Err(Error::from_hresult(windows::core::HRESULT(
            0x80070057u32 as i32,
        ))),
    })
}

#[no_mangle]
pub unsafe extern "C" fn lummix_media_player_snapshot(preferred: *const c_char) -> *mut c_char {
    response(|| {
        let preferred = (!preferred.is_null())
            .then(|| unsafe { CStr::from_ptr(preferred) })
            .and_then(|value| value.to_str().ok())
            .map(std::path::Path::new);
        player::snapshot(preferred)
    })
}

#[no_mangle]
pub extern "C" fn lummix_media_listen(
    id: u64,
    kind: u32,
    callback: MediaEvent,
    event_id: u64,
) -> *mut c_char {
    response(|| add_event(id, kind, callback, event_id))
}

#[no_mangle]
pub unsafe extern "C" fn lummix_media_volume(request: *const c_char) -> *mut c_char {
    response(|| {
        if !request.is_null() {
            if let Ok(request) =
                serde_json::from_slice(unsafe { CStr::from_ptr(request).to_bytes() })
            {
                return volume::player_volume(&request);
            }
        }
        Ok(json!({"reason": "slider_unavailable", "http": 503}))
    })
}

#[no_mangle]
pub unsafe extern "C" fn lummix_media_cable(
    root_pid: u32,
    signed_ids: *const c_char,
) -> *mut c_char {
    response(|| {
        if !signed_ids.is_null() {
            if let Ok(ids) = serde_json::from_slice::<Vec<String>>(unsafe {
                CStr::from_ptr(signed_ids).to_bytes()
            }) {
                return audio_sessions::direct_to_cable(root_pid, &ids).map(|state| json!(state));
            }
        }
        Ok(Value::Null)
    })
}

#[no_mangle]
pub unsafe extern "C" fn lummix_media_peak(processes: *const c_char) -> *mut c_char {
    response(|| {
        if !processes.is_null() {
            if let Ok(processes) = serde_json::from_slice::<Vec<(u32, f64)>>(unsafe {
                CStr::from_ptr(processes).to_bytes()
            }) {
                return audio_sessions::peak(&processes);
            }
        }
        Ok(Value::Null)
    })
}

#[no_mangle]
pub unsafe extern "C" fn lummix_media_capability(
    op: u32,
    store: u64,
    request: *const c_char,
) -> *mut c_char {
    let request = if request.is_null() {
        Value::Null
    } else {
        serde_json::from_slice(unsafe { CStr::from_ptr(request).to_bytes() }).unwrap_or(Value::Null)
    };
    encode_response(capability::apply(op, store, &request))
}

#[no_mangle]
pub unsafe extern "C" fn lummix_spotify_setup(
    op: u32,
    request: *const c_char,
    callback: Option<extern "C" fn(u32) -> bool>,
) -> *mut c_char {
    let input = (!request.is_null())
        .then(|| unsafe { CStr::from_ptr(request).to_bytes() })
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok());
    let result = (|| {
        let input = input
            .as_ref()
            .ok_or_else(|| std::io::Error::other("invalid setup request"))?;
        let root = input["root"]
            .as_str()
            .ok_or_else(|| std::io::Error::other("missing setup location"))?;
        let preferred = input["preferred"].as_str().map(std::path::Path::new);
        let _apartment = if op == 1 || op == 2 {
            Some(Apartment::enter().map_err(std::io::Error::other)?)
        } else {
            None
        };
        setup::run(op, std::path::Path::new(root), preferred, &mut |phase| {
            callback.is_some_and(|notify| notify(phase))
        })
    })();
    let payload = match result {
        Ok(value) => json!({"value": value}),
        Err(error) => json!({"error": error.to_string()}),
    };
    CString::new(payload.to_string()).unwrap().into_raw()
}

/// Verify a single Spotify source and its render route without opening capture.
#[no_mangle]
pub unsafe extern "C" fn lummix_audio_source(
    signed_render_ids_json: *const c_char,
    pid: *mut u32,
    created: *mut f64,
    edition: *mut u32,
) -> i32 {
    if signed_render_ids_json.is_null() || pid.is_null() || created.is_null() || edition.is_null() {
        return 4;
    }
    unsafe {
        *pid = 0;
        *created = 0.0;
        *edition = 0;
    }
    let Ok(ids) = serde_json::from_slice::<Vec<String>>(unsafe {
        CStr::from_ptr(signed_render_ids_json).to_bytes()
    }) else {
        return 4;
    };
    let Ok(_apartment) = Apartment::enter() else {
        return 4;
    };
    let Ok((state, target)) = source::probe(&ids) else {
        return 4;
    };
    if let Some((target_pid, target_created, target_edition)) = target {
        unsafe {
            *pid = target_pid;
            *created = target_created;
            *edition = target_edition;
        }
    }
    state
}

#[no_mangle]
pub extern "C" fn lummix_media_release_session(id: u64) {
    if let Ok(_apartment) = Apartment::enter() {
        media().sessions.remove(&id);
    }
}

#[no_mangle]
pub unsafe extern "C" fn lummix_media_free(data: *mut c_char) {
    if !data.is_null() {
        drop(unsafe { CString::from_raw(data) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_sole_session_or_sole_playing_session() {
        assert_eq!(choose_session(vec![false], |_| Ok(false)), Ok(false));
        assert_eq!(
            choose_session(vec![false, true], |playing| Ok(*playing)),
            Ok(true)
        );
        assert_eq!(
            choose_session(vec![true, true], |playing| Ok(*playing)),
            Err("ambiguous")
        );
        assert_eq!(
            choose_session(Vec::<bool>::new(), |_| Ok(false)),
            Err("no_session")
        );
    }

    #[test]
    fn projects_only_playing_timeline_and_clamps_its_span() {
        let value = timeline_values((3.0, 10.0), (2.0, 7.0), 5.0, 2.0, true, Some(8.0), 11.0);
        assert_eq!(value["position"], 10.0);
        assert_eq!(value["minSeek"], 3.0);
        assert_eq!(value["maxSeek"], 7.0);
        let value = timeline_values(
            (3.0, 10.0),
            (11.0, 12.0),
            5.0,
            f64::NAN,
            false,
            Some(8.0),
            11.0,
        );
        assert_eq!(value["position"], 5.0);
        assert_eq!(value["rate"], 1.0);
        assert!(value["minSeek"].is_null());
    }

    #[test]
    fn recognizes_only_verified_spotify_source_ids() {
        assert!(spotify_source("Spotify.EXE"));
        assert!(spotify_source(
            "spotifyab.spotifymusic_zpdnekdrzrea0!spotify"
        ));
        for source in [
            "",
            "Spotify",
            "myspotify.exe",
            "Spotify.exe.Web",
            STORE_FAMILY,
            "SpotifyAB.SpotifyMusic_other!Spotify",
        ] {
            assert!(!spotify_source(source), "{source}");
        }
    }
}
