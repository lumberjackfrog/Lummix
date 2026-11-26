//! Spotify-only loopback routes. The host is responsible for HTTP limits, Host validation and CORS.
use super::{
    artwork, audio_sessions, capability, change_feed::Feed, player, processes, session, setup,
    source, volume, window_player, Apartment, PlaybackStatus, STORE_PLAYER,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/spotify/status"),
    ("GET", "/spotify/changes"),
    ("GET", "/spotify/peak"),
    ("GET", "/spotify/volume"),
    ("POST", "/spotify/volume"),
    ("POST", "/spotify/control"),
];

pub const CAPABILITIES: &[&str] = &[
    "spotify-detection-v2",
    "spotify-window-artwork-v1",
    "spotify-smtc-rust-v1",
    "spotify-player-volume-v1",
    "spotify-session-peak-v1",
];

const TOKEN_TTL: Duration = Duration::from_secs(600);
const TOKEN_LIMIT: usize = 64;

type Response = (u16, Vec<u8>);
type Remember = Box<dyn Fn(&str, &Path) + Send + Sync>;

fn json_response(code: u16, body: Value) -> Response {
    (code, body.to_string().into_bytes())
}

fn empty(code: u16) -> Response {
    (code, Vec::new())
}

fn unavailable(code: u16, reason: &str) -> Response {
    json_response(code, json!({"state":"unavailable","error":reason}))
}

fn status_fields(fields: Value) -> Value {
    let mut result = json!({
        "state":"unavailable","playing":false,"canPlay":false,"canPause":false,
        "canNext":false,"canPrevious":false,"canStop":false,"canSeek":false,
        "timeline":null,"title":"","artist":"","token":null,"edition":"unknown",
        "installations":[],"installationKnown":false,"artwork":null,"artworkId":null,
    });
    if let Value::Object(fields) = fields {
        result.as_object_mut().unwrap().extend(fields);
    }
    result
}

fn status_error(code: u16, state: &str, reason: &str) -> Response {
    json_response(code, status_fields(json!({"state":state,"error":reason})))
}

fn one_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    let mut values = headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name));
    let value = values.next()?.1.as_str();
    values.next().is_none().then_some(value)
}

fn content_is_json(headers: &[(String, String)]) -> bool {
    one_header(headers, "Content-Type")
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
}

fn token_from(headers: &[(String, String)]) -> Option<&str> {
    one_header(headers, "X-Lum-Spotify-Token").filter(|token| {
        token.len() == 43
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

fn unescape(query: &str) -> String {
    let raw = query.as_bytes();
    let mut output = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        match raw[index] {
            b'+' => output.push(b' '),
            b'%' if index + 2 < raw.len() => {
                if let (Some(hi), Some(lo)) = (
                    (raw[index + 1] as char).to_digit(16),
                    (raw[index + 2] as char).to_digit(16),
                ) {
                    output.push((hi * 16 + lo) as u8);
                    index += 3;
                    continue;
                }
                output.push(b'%');
            }
            other => output.push(other),
        }
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.trim_start_matches('?').split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        (unescape(name) == key).then(|| unescape(value))
    })
}

fn single_field(body: &[u8]) -> Option<Value> {
    let data: Value = serde_json::from_slice(body).ok()?;
    if data.as_object()?.len() != 1 {
        return None;
    }
    // The supported fields have scalar values. A comma outside a JSON string
    // would indicate an extra field, including duplicate keys.
    let mut quoted = false;
    let mut escaped = false;
    for &byte in body {
        if escaped {
            escaped = false;
        } else if quoted && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if !quoted && byte == b',' {
            return None;
        }
    }
    Some(data)
}

fn valid_volume(change: &Value) -> bool {
    change["muted"].is_boolean()
        || change["volume"]
            .as_f64()
            .is_some_and(|value| (0.0..=100.0).contains(&value))
}

fn valid_control(body: &Value) -> Option<u32> {
    let action = body["action"].as_str()?;
    [
        "play",
        "pause",
        "previous",
        "next",
        "stop",
        "seekBack15",
        "seekForward15",
    ]
    .iter()
    .position(|command| *command == action)
    .map(|index| index as u32)
}

#[derive(Clone, Copy)]
struct Window {
    handle: u64,
    pid: u32,
    created: f64,
    edition: &'static str,
}

impl Window {
    fn from_json(value: &Value) -> Option<Self> {
        let edition = match value[0].as_str()? {
            "desktop" => "desktop",
            "store" => "store",
            _ => return None,
        };
        let identity = &value[1];
        Some(Self {
            handle: identity[0].as_u64()?,
            pid: u32::try_from(identity[1].as_u64()?).ok()?,
            created: identity[2].as_f64()?,
            edition,
        })
    }

    fn current(self) -> windows::core::Result<bool> {
        let windows = windows_for_edition(self.edition)?;
        let [current] = windows.as_slice() else {
            return Ok(false);
        };
        Ok(current.handle == self.handle
            && current.pid == self.pid
            && current.created == self.created
            && processes::matches_creation(self.pid, self.created))
    }
}

fn windows_for_edition(edition: &str) -> windows::core::Result<Vec<Window>> {
    let (edition, edition_id) = if edition == "store" {
        ("store", 2)
    } else {
        ("desktop", 1)
    };
    let processes = source::verified_processes()?;
    let windows = window_player::visible_windows()?;
    Ok(windows
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|window| {
            let pid = u32::try_from(window[1].as_u64()?).ok()?;
            let (process, _) = processes
                .iter()
                .find(|(process, current)| process.pid == pid && *current == edition_id)?;
            Some(Window {
                handle: window[0].as_u64()?,
                pid,
                created: process.created,
                edition,
            })
        })
        .collect())
}

enum Target {
    Native { id: u64, edition: &'static str },
    Window(Window),
}

impl Drop for Target {
    fn drop(&mut self) {
        if let Self::Native { id, .. } = self {
            // WinRT media sessions are agile and the retained reference has no
            // thread affinity. Drop it outside the registry lock.
            let target = super::media().sessions.remove(id);
            drop(target);
        }
    }
}

struct Lease {
    token: String,
    origin: String,
    expires: Instant,
    target: Arc<Target>,
    volume_binding: Option<Value>,
}

#[derive(Default)]
struct Peak {
    edition: Option<&'static str>,
    refresh: Option<Instant>,
    sampled: Option<Instant>,
    processes: Vec<(u32, f64)>,
    value: Value,
}

impl Peak {
    fn read(&mut self, edition: &'static str) -> Value {
        let now = Instant::now();
        let unchanged = self.edition == Some(edition);
        if unchanged
            && self
                .sampled
                .is_some_and(|sample| now.duration_since(sample) < Duration::from_millis(150))
        {
            return self.value.clone();
        }
        if !unchanged || self.refresh.is_none_or(|time| now >= time) {
            let edition_id = if edition == "store" { 2 } else { 1 };
            self.processes = source::verified_processes()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(process, actual)| {
                    (actual == edition_id).then_some((process.pid, process.created))
                })
                .collect();
            self.edition = Some(edition);
            self.refresh = Some(now + Duration::from_secs(2));
        }
        self.sampled = Some(now);
        self.value = audio_sessions::peak(&self.processes).unwrap_or(Value::Null);
        self.value.clone()
    }
}

struct FallbackCover {
    item: String,
    retry_at: Instant,
    artwork: Value,
}

pub struct PlayerApi {
    preferred: Mutex<Option<PathBuf>>,
    remember: Remember,
    inventory_revision: Mutex<u64>,
    leases: Mutex<VecDeque<Lease>>,
    control_lock: Mutex<()>,
    volume_lock: Mutex<()>,
    feed: Mutex<Option<Arc<Feed>>>,
    prefix: String,
    peak: Mutex<Peak>,
    cover: Mutex<Option<FallbackCover>>,
    closed: AtomicBool,
}

impl PlayerApi {
    pub fn new(profile_dir: &Path) -> Result<Self, String> {
        let root = profile_dir.to_owned();
        Self::with_profile_hooks(profile_dir, move || read_desktop(&root), |_, _| {})
    }

    pub fn with_profile_hooks(
        _profile_dir: &Path,
        read_desktop: impl Fn() -> Option<PathBuf> + Send + Sync + 'static,
        remember: impl Fn(&str, &Path) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let prefix =
            capability::random_token().map_err(|error| error.to_string())?[..16].to_owned();
        Ok(Self {
            preferred: Mutex::new(read_desktop()),
            remember: Box::new(remember),
            inventory_revision: Mutex::new(0),
            leases: Mutex::new(VecDeque::new()),
            control_lock: Mutex::new(()),
            volume_lock: Mutex::new(()),
            feed: Mutex::new(None),
            prefix,
            peak: Mutex::new(Peak::default()),
            cover: Mutex::new(None),
            closed: AtomicBool::new(false),
        })
    }

    fn issue(&self, target: Target, origin: &str) -> Option<String> {
        let token = capability::random_token().ok()?;
        let now = Instant::now();
        let mut leases = self.leases.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        leases.retain(|lease| lease.expires > now);
        while leases.len() >= TOKEN_LIMIT {
            leases.pop_front();
        }
        leases.push_back(Lease {
            token: token.clone(),
            origin: origin.to_owned(),
            expires: now + TOKEN_TTL,
            target: Arc::new(target),
            volume_binding: None,
        });
        Some(token)
    }

    fn find(&self, token: &str, origin: &str) -> Option<(Arc<Target>, Option<Value>)> {
        let leases = self.leases.lock().unwrap();
        leases
            .iter()
            .find(|lease| {
                lease.origin == origin
                    && lease.expires > Instant::now()
                    && token.len() == lease.token.len()
                    && token
                        .bytes()
                        .zip(lease.token.bytes())
                        .fold(0, |diff, (a, b)| diff | (a ^ b))
                        == 0
            })
            .map(|lease| (lease.target.clone(), lease.volume_binding.clone()))
    }

    fn revoke(&self, token: &str) {
        self.leases
            .lock()
            .unwrap()
            .retain(|lease| lease.token != token);
    }

    fn update_binding(&self, token: &str, binding: Option<Value>) {
        if let Some(lease) = self
            .leases
            .lock()
            .unwrap()
            .iter_mut()
            .find(|lease| lease.token == token)
        {
            lease.volume_binding = binding;
        }
    }

    fn preserve_inventory(&self, status: &mut Value) {
        let fields = status.as_object_mut().unwrap();
        let revision = fields.remove("inventoryRevision").and_then(|v| v.as_u64());
        let paths = fields.remove("inventoryPaths");
        let mut current = self.inventory_revision.lock().unwrap();
        if revision.is_some_and(|revision| revision > *current) {
            *current = revision.unwrap();
            if let Some(paths) = paths {
                for edition in ["desktop", "store"] {
                    if let Some(path) = paths[edition].as_str() {
                        if edition == "desktop" {
                            *self.preferred.lock().unwrap() = Some(PathBuf::from(path));
                        }
                        (self.remember)(edition, Path::new(path));
                    }
                }
            }
        }
    }

    fn cover(&self, item: &str) -> Value {
        let Some(item) = artwork::item_url(item) else {
            return Value::Null;
        };
        let mut cache = self.cover.lock().unwrap();
        let now = Instant::now();
        if let Some(entry) = cache.as_ref() {
            if entry.item == item && (!entry.artwork.is_null() || now < entry.retry_at) {
                return entry.artwork.clone();
            }
        }
        let value = artwork::fetch_cover(&item)
            .and_then(|raw| player::encode_cover(&raw))
            .unwrap_or(Value::Null);
        *cache = Some(FallbackCover {
            item,
            retry_at: now + Duration::from_secs(20),
            artwork: value.clone(),
        });
        value
    }

    fn snapshot(&self, origin: &str, previous_artwork: Option<&str>) -> Value {
        let preferred = self.preferred.lock().unwrap().clone();
        let Ok(mut fields) = player::snapshot(preferred.as_deref()) else {
            return status_fields(Value::Null);
        };
        self.preserve_inventory(&mut fields);
        let target = fields.as_object_mut().and_then(|fields| {
            if let Some(session) = fields.remove("session") {
                let id = session[0].as_u64()?;
                let edition = if session[1]
                    .as_str()
                    .is_some_and(|source| source.eq_ignore_ascii_case(STORE_PLAYER))
                {
                    "store"
                } else {
                    "desktop"
                };
                Some(Target::Native { id, edition })
            } else {
                Window::from_json(&fields.remove("window")?).map(Target::Window)
            }
        });
        if let Some(target) = target {
            fields["token"] = json!(self.issue(target, origin));
        }
        if fields["artwork"].is_null() {
            if let Some(item) = fields["itemUrl"].as_str() {
                if let Value::Object(cover) = self.cover(item) {
                    fields.as_object_mut().unwrap().extend(cover);
                }
            }
        }
        let mut result = status_fields(fields);
        if !result["artworkId"].is_null() && result["artworkId"].as_str() == previous_artwork {
            result.as_object_mut().unwrap().remove("artwork");
        }
        result
    }

    fn command(&self, origin: &str, headers: &[(String, String)], body: &[u8]) -> Response {
        let Some(token) = token_from(headers) else {
            return status_error(403, "unavailable", "refresh_selection");
        };
        if !content_is_json(headers) {
            return empty(415);
        }
        let Some(action) = single_field(body).and_then(|data| valid_control(&data)) else {
            return empty(400);
        };
        let _guard = self.control_lock.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            return empty(503);
        }
        let Some((target, _)) = self.find(token, origin) else {
            return status_error(403, "unavailable", "refresh_selection");
        };
        match target.as_ref() {
            Target::Native { id, .. } => {
                let Ok(result) = player::command(*id, action) else {
                    return status_error(503, "unavailable", "check_player");
                };
                let code = result["status"].as_u64().unwrap_or(503) as u16;
                if result["error"] == "selection_changed" {
                    self.revoke(token);
                }
                if result["refresh"] == true {
                    let mut status = self.snapshot(origin, None);
                    if !result["error"].is_null() {
                        status["error"] = result["error"].clone();
                    }
                    return json_response(code, status);
                }
                status_error(
                    code,
                    result["state"].as_str().unwrap_or("unavailable"),
                    result["error"].as_str().unwrap_or("check_player"),
                )
            }
            Target::Window(window) => {
                match window.current() {
                    Ok(true) => {}
                    Ok(false) => return status_error(409, "unavailable", "selection_changed"),
                    Err(_) => return status_error(503, "unavailable", "window_unavailable"),
                }
                let native_action = match action {
                    0..=3 => action + 1,
                    _ => 5,
                };
                match window_player::transport(window.handle, window.pid, native_action) {
                    Ok(result) if result["reason"].is_null() => {
                        json_response(200, self.snapshot(origin, None))
                    }
                    Ok(result) => status_error(
                        409,
                        "unavailable",
                        result["reason"].as_str().unwrap_or("no_player_controls"),
                    ),
                    Err(_) => status_error(409, "unavailable", "no_player_controls"),
                }
            }
        }
    }

    fn read_peak(&self, origin: &str, headers: &[(String, String)]) -> Response {
        let target = token_from(headers).and_then(|token| self.find(token, origin));
        let Some((target, _)) = target else {
            return unavailable(403, "refresh_selection");
        };
        let edition = match target.as_ref() {
            Target::Window(window) => match window.current() {
                Ok(true) => window.edition,
                Ok(false) => return unavailable(409, "selection_changed"),
                Err(_) => return json_response(200, json!({"state":"no_local_signal"})),
            },
            Target::Native { id, edition } => {
                if !player::selection_matches(*id).is_ok_and(|matches| matches == true) {
                    return unavailable(409, "selection_changed");
                }
                match session(*id)
                    .and_then(|session| session.GetPlaybackInfo())
                    .and_then(|info| info.PlaybackStatus())
                {
                    Ok(status) if status == PlaybackStatus::Playing => edition,
                    Ok(_) => return json_response(200, json!({"state":"paused"})),
                    Err(_) => return json_response(200, json!({"state":"no_local_signal"})),
                }
            }
        };
        let value = self.peak.lock().unwrap().read(edition);
        let state = if let Some(level) = value.as_f64() {
            json!({"state":"ready","peak":(level * 1000.0).round_ties_even() / 1000.0})
        } else {
            json!({"state":"no_local_signal"})
        };
        json_response(200, state)
    }

    fn volume(
        &self,
        method: &str,
        origin: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Response {
        let Some(token) = token_from(headers) else {
            return unavailable(403, "refresh_selection");
        };
        let change = if method == "POST" {
            if !content_is_json(headers) {
                return json_response(415, json!({"error":"expected_json"}));
            }
            let Some(change) = single_field(body).filter(valid_volume) else {
                return json_response(400, json!({"error":"invalid_command"}));
            };
            change
        } else {
            Value::Null
        };
        let _guard = self.volume_lock.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            return empty(503);
        }
        let Some((target, binding)) = self.find(token, origin) else {
            return unavailable(403, "refresh_selection");
        };
        let Target::Native { id, edition } = target.as_ref() else {
            return unavailable(409, "windows_media_unavailable");
        };
        if !player::selection_matches(*id).is_ok_and(|matches| matches == true) {
            self.update_binding(token, None);
            return unavailable(409, "selection_changed");
        }
        if !change.is_null() && binding.is_none() {
            return unavailable(409, "read_volume_first");
        }
        let windows = match windows_for_edition(edition) {
            Ok(windows) => windows
                .into_iter()
                .map(|window| json!([window.handle, window.pid, window.created]))
                .collect::<Vec<_>>(),
            Err(_) => {
                self.update_binding(token, None);
                return unavailable(503, "window_unavailable");
            }
        };
        match volume::player_volume(&json!({"windows":windows,"expected":binding,"change":change}))
        {
            Ok(result) if result.is_array() => {
                self.update_binding(token, Some(result[0].clone()));
                json_response(200, result[1].clone())
            }
            Ok(result) => {
                self.update_binding(token, None);
                unavailable(
                    result["http"].as_u64().unwrap_or(409) as u16,
                    result["reason"].as_str().unwrap_or("slider_unavailable"),
                )
            }
            Err(_) => {
                self.update_binding(token, None);
                unavailable(503, "slider_unavailable")
            }
        }
    }

    fn changes(&self, since: Option<&str>) -> Response {
        let feed = {
            let mut feed = self.feed.lock().unwrap();
            if self.closed.load(Ordering::Acquire) {
                return empty(503);
            }
            if feed.is_none() {
                *feed = Feed::new(self.prefix.clone(), None).ok().map(Arc::new);
            }
            feed.clone()
        };
        json_response(
            200,
            feed.map_or_else(
                || json!({"events":false,"revision":format!("{}.0",self.prefix)}),
                |feed| feed.read(since),
            ),
        )
    }

    pub fn handle(
        &self,
        method: &str,
        path: &str,
        origin: &str,
        query: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Response {
        if !matches!(origin, "http://127.0.0.1:41835" | "http://localhost:41835") {
            return json_response(403, json!({"error":"unauthorized_origin"}));
        }
        if self.closed.load(Ordering::Acquire) {
            return empty(503);
        }
        if !ROUTES
            .iter()
            .any(|(verb, route)| *verb == method && *route == path)
        {
            return empty(if ROUTES.iter().any(|(_, route)| *route == path) {
                405
            } else {
                404
            });
        }
        if path == "/spotify/changes" {
            return self.changes(query_param(query, "since").as_deref());
        }
        let unavailable = || match path {
            "/spotify/status" => json_response(200, status_fields(Value::Null)),
            "/spotify/control" => status_error(503, "unavailable", "check_player"),
            _ => unavailable(503, "check_player"),
        };
        match Apartment::enter() {
            Ok(_apartment) => self.dispatch(method, path, origin, query, headers, body),
            Err(error) if error.code() == windows::Win32::Foundation::RPC_E_CHANGED_MODE => {
                std::thread::scope(|scope| {
                    std::thread::Builder::new()
                        .name("Lummix Spotify request".into())
                        .spawn_scoped(scope, || {
                            let Ok(_apartment) = Apartment::enter() else {
                                return unavailable();
                            };
                            self.dispatch(method, path, origin, query, headers, body)
                        })
                        .ok()
                        .and_then(|thread| thread.join().ok())
                        .unwrap_or_else(unavailable)
                })
            }
            Err(_) => unavailable(),
        }
    }

    fn dispatch(
        &self,
        method: &str,
        path: &str,
        origin: &str,
        query: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Response {
        match path {
            "/spotify/status" => json_response(
                200,
                self.snapshot(origin, query_param(query, "artworkId").as_deref()),
            ),
            "/spotify/control" => self.command(origin, headers, body),
            "/spotify/peak" => self.read_peak(origin, headers),
            "/spotify/volume" => self.volume(method, origin, headers, body),
            _ => empty(404),
        }
    }

    pub fn shutdown(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(feed) = self.feed.lock().unwrap().take() {
            feed.shutdown();
        }
        self.leases.lock().unwrap().clear();
    }
}

impl Drop for PlayerApi {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn read_desktop(root: &Path) -> Option<PathBuf> {
    fn small_json(path: &Path) -> Option<Value> {
        use std::io::Read;
        let file = std::fs::File::open(path).ok()?;
        let mut bytes = Vec::new();
        file.take(256 * 1024 + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() > 256 * 1024 {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }
    let path = |value: &Value| {
        let path = PathBuf::from(value["desktop"].as_str()?);
        source::normalize(&path).map(|_| path)
    };
    small_json(&root.join("spotify-locations.json"))
        .as_ref()
        .and_then(path)
        .or_else(|| {
            let backup = small_json(&root.join("settings-backup.json"))?;
            path(&backup["settings"]["spotify-locations.json"])
        })
}

/// Perform the explicit one-click setup from a worker. Empty progress checks cancellation.
pub fn spotify_setup(
    profile_dir: &Path,
    preferred_desktop: Option<&Path>,
    mut progress: impl FnMut(&str) -> bool,
) -> Result<String, String> {
    let _apartment = Apartment::enter().map_err(|error| error.to_string())?;
    let info = setup::run(1, profile_dir, preferred_desktop, &mut |phase| {
        progress(match phase {
            1 => "downloading",
            2 => "installing",
            _ => "",
        })
    })
    .map_err(|error| error.to_string())?;
    info.as_str()
        .map(str::to_owned)
        .ok_or_else(|| "Invalid Spotify setup response".to_owned())
}

pub fn spotify_setup_info(
    profile_dir: &Path,
    preferred_desktop: Option<&Path>,
) -> Result<Value, String> {
    let _apartment = Apartment::enter().map_err(|error| error.to_string())?;
    setup::run(2, profile_dir, preferred_desktop, &mut |_| true).map_err(|error| error.to_string())
}

pub fn spotify_setup_cleanup(profile_dir: &Path) {
    let _ = setup::run(0, profile_dir, None, &mut |_| false);
}

pub fn diagnostic_target() -> Result<Value, String> {
    let _apartment = Apartment::enter().map_err(|error| error.to_string())?;
    source::diagnostic_target().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STUDIO: &str = "http://127.0.0.1:41835";
    const OTHER_LOOPBACK: &str = "http://localhost:41835";

    fn fixture_api() -> PlayerApi {
        PlayerApi::new(&std::env::temp_dir().join("lummix-spotify-api-fixture")).unwrap()
    }

    fn headers(token: &str) -> Vec<(String, String)> {
        vec![
            ("X-Lum-Spotify-Token".into(), token.into()),
            (
                "content-type".into(),
                "application/json; charset=utf-8".into(),
            ),
        ]
    }

    fn decode(body: &Response) -> Value {
        serde_json::from_slice(&body.1).unwrap()
    }

    #[test]
    fn origin_capabilities_and_command_bodies_are_guarded_without_touching_a_player() {
        let api = fixture_api();
        let window = Window {
            handle: 0,
            pid: 0,
            created: 0.0,
            edition: "desktop",
        };
        let token = api.issue(Target::Window(window), STUDIO).unwrap();
        let command = br#"{"action":"next"}"#;
        let call = |origin, headers: &[(String, String)], body: &[u8]| {
            api.handle("POST", "/spotify/control", origin, "", headers, body)
        };
        assert_eq!(
            call("https://external.invalid", &headers(&token), command).0,
            403
        );
        assert_eq!(call(OTHER_LOOPBACK, &headers(&token), command).0, 403);
        assert_eq!(call(STUDIO, &[], command).0, 403);
        let mut duplicate = headers(&token);
        duplicate.push(("x-lum-spotify-token".into(), token.clone()));
        assert_eq!(call(STUDIO, &duplicate, command).0, 403);
        assert_eq!(
            call(
                STUDIO,
                &headers(&token),
                br#"{"action":"play","action":"next"}"#
            )
            .0,
            400
        );
        let mut no_type = headers(&token);
        no_type.pop();
        assert_eq!(call(STUDIO, &no_type, command).0, 415);
        let stale = call(STUDIO, &headers(&token), command);
        assert_eq!(stale.0, 409);
        assert_eq!(decode(&stale)["error"], "selection_changed");
        let volume = api.handle("GET", "/spotify/volume", STUDIO, "", &headers(&token), b"");
        assert_eq!(volume.0, 409);
        assert_eq!(decode(&volume)["error"], "windows_media_unavailable");
        assert_eq!(
            api.handle(
                "POST",
                "/spotify/volume",
                STUDIO,
                "",
                &headers(&token),
                br#"{"volume":true}"#
            )
            .0,
            400
        );
        api.shutdown();
        assert_eq!(call(STUDIO, &headers(&token), command).0, 503);
    }

    #[test]
    fn query_decodes_the_first_value_and_requires_exact_route_methods() {
        assert_eq!(
            query_param("artwork%49d=A%2BB&artworkId=last", "artworkId").as_deref(),
            Some("A+B")
        );
        let api = fixture_api();
        assert_eq!(
            api.handle("HEAD", "/spotify/status", STUDIO, "", &[], b"")
                .0,
            405
        );
        assert_eq!(
            api.handle("GET", "/duck/status", STUDIO, "", &[], b"").0,
            404
        );
        let initial = api.handle("GET", "/spotify/changes", STUDIO, "", &[], b"");
        assert_eq!(initial.0, 200);
        assert!(decode(&initial)["revision"].is_string());
    }

    #[test]
    #[ignore = "read-only live Spotify metadata and latency"]
    fn reads_real_status_without_sending_player_commands() {
        let api = fixture_api();
        let mut times = Vec::new();
        let mut status = Value::Null;
        for _ in 0..24 {
            let begin = Instant::now();
            let reply = api.handle("GET", "/spotify/status", STUDIO, "", &[], b"");
            times.push(begin.elapsed());
            assert_eq!(reply.0, 200);
            status = decode(&reply);
            assert_eq!(status["state"], "ready");
            assert!(status.get("inventoryPaths").is_none());
        }
        times.sort();
        println!(
            "Spotify read-only metadata p50 {:?}, p95 {:?}; edition {:?}, art present {}",
            times[12],
            times[22],
            status["edition"],
            status["artwork"].is_string()
        );
        let token = status["token"].as_str().unwrap();
        let peak = api.handle("GET", "/spotify/peak", STUDIO, "", &headers(token), b"");
        assert_eq!(peak.0, 200);
        assert_eq!(
            api.handle(
                "GET",
                "/spotify/peak",
                OTHER_LOOPBACK,
                "",
                &headers(token),
                b""
            )
            .0,
            403
        );
        if let Some(artwork_id) = status["artworkId"].as_str() {
            let refreshed = api.handle(
                "GET",
                "/spotify/status",
                STUDIO,
                &format!("artworkId={artwork_id}"),
                &[],
                b"",
            );
            let refreshed = decode(&refreshed);
            if refreshed["artworkId"] == artwork_id {
                assert!(refreshed.get("artwork").is_none());
            }
        }
    }

    #[test]
    #[ignore = "read-only public Spotify oEmbed and CDN probe"]
    fn fetches_public_cover_via_native_https() {
        let _apartment = Apartment::enter().unwrap();
        let bytes =
            artwork::fetch_cover("https://open.spotify.com/album/7b5gKWCRwV2ndFikqNJN89").unwrap();
        assert!(bytes.len() <= artwork::MAX_BYTES);
        let encoded = player::encode_cover(&bytes).unwrap();
        assert!(encoded["artwork"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,"));
        println!(
            "Validated native oEmbed artwork source bytes {}",
            bytes.len()
        );
    }
}
