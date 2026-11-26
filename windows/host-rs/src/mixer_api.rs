use crate::media_http::{payload_limit, NativeBody};
use crate::{profile, setup_runtime};
use lummix_dsp::{
    control_plane::{valid_profile, ControlState},
    runtime::{self, Controls, LiveRoute},
};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/mix/gains"),
    ("POST", "/mix/gains"),
    ("GET", "/mix/duck"),
    ("POST", "/mix/duck"),
    ("GET", "/mix/mic"),
    ("POST", "/mix/mic"),
    ("GET", "/mix/preferences"),
    ("POST", "/mix/preferences"),
];
type Reply = (u16, Value);
type Checked<T> = Result<T, Reply>;
fn failure(status: u16, code: &str) -> Reply {
    (status, json!({"error":code}))
}
pub(crate) fn response_body(status: u16, body: Value) -> Vec<u8> {
    if status == 413 {
        body.as_str().unwrap_or_default().as_bytes().to_vec()
    } else if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    }
}

fn decode_charset<'a>(mime: &str, bytes: &'a [u8]) -> Option<Cow<'a, [u8]>> {
    let charset = mime
        .split(';')
        .skip(1)
        .filter_map(|field| field.split_once('='))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("charset"))
        .map_or("utf-8", |(_, value)| value.trim().trim_matches('"'));
    let encoding = charset.to_ascii_lowercase().replace(['-', '_'], "");
    let text = match encoding.as_str() {
        "utf8" | "utf8sig" => {
            let bom = bytes.strip_prefix(b"\xef\xbb\xbf");
            let input = if encoding == "utf8" { bytes } else { bom.unwrap_or(bytes) };
            return (!input.starts_with(b"\xef\xbb\xbf")).then_some(Cow::Borrowed(input));
        }
        "ascii" | "usascii" if bytes.is_ascii() => return Some(Cow::Borrowed(bytes)),
        "iso88591" | "latin1" | "latin" => bytes.iter().copied().map(char::from).collect(),
        "utf16" | "utf16le" | "utf16be" => {
            let (input, big_endian) = if encoding == "utf16" {
                if let Some(input) = bytes.strip_prefix(b"\xfe\xff") {
                    (input, true)
                } else {
                    (bytes.strip_prefix(b"\xff\xfe").unwrap_or(bytes), false)
                }
            } else {
                (bytes, encoding == "utf16be")
            };
            let mut chunks = input.chunks_exact(2);
            let words: Vec<u16> = chunks
                .by_ref()
                .map(|pair| {
                    let pair = [pair[0], pair[1]];
                    if big_endian {
                        u16::from_be_bytes(pair)
                    } else {
                        u16::from_le_bytes(pair)
                    }
                })
                .collect();
            if !chunks.remainder().is_empty() {
                return None;
            }
            String::from_utf16(&words).ok()?
        }
        "utf32" | "utf32le" | "utf32be" => {
            let (input, big_endian) = if encoding == "utf32" {
                if let Some(input) = bytes.strip_prefix(b"\0\0\xfe\xff") {
                    (input, true)
                } else {
                    (bytes.strip_prefix(b"\xff\xfe\0\0").unwrap_or(bytes), false)
                }
            } else {
                (bytes, encoding == "utf32be")
            };
            let mut chunks = input.chunks_exact(4);
            let text = chunks
                .by_ref()
                .map(|word| {
                    let word = [word[0], word[1], word[2], word[3]];
                    char::from_u32(if big_endian {
                        u32::from_be_bytes(word)
                    } else {
                        u32::from_le_bytes(word)
                    })
                })
                .collect::<Option<String>>()?;
            if !chunks.remainder().is_empty() {
                return None;
            }
            text
        }
        _ => return None,
    };
    // File recovery permits a UTF-8 BOM. HTTP JSON only permits it via a
    // charset that consumed the corresponding encoding signature above.
    if text.starts_with('\u{feff}') {
        return None;
    }
    Some(Cow::Owned(text.into_bytes()))
}
fn op(c: &mut ControlState, request: Value) -> Checked<Value> {
    c.call(&request).map_err(|e| {
        failure(
            if e == "settings_unavailable" {
                503
            } else {
                400
            },
            e,
        )
    })
}
fn object_merge(mut a: Value, b: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (a.as_object_mut(), b) {
        base.extend(extra);
    }
    a
}
fn list(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect()
}
struct State {
    control: ControlState,
    route: Option<LiveRoute>,
}
impl State {
    fn controls(&mut self) -> Checked<Controls> {
        let gains = op(&mut self.control, json!({"op":"gain_get"}))?;
        let g = &gains["gains"];
        let d = op(&mut self.control, json!({"op":"duck_get"}))?;
        Ok(Controls {
            mic: g["micGain"].as_f64().unwrap_or(100.) / 100.,
            music: g["musicGain"].as_f64().unwrap_or(100.) / 100.,
            depth: d["settings"]["depthDb"].as_f64().unwrap_or(12.),
            flags: (g["micMuted"] == true) as u32
                | (((g["musicMuted"] == true) as u32) << 1)
                | (((d["settings"]["enabled"] == true) as u32) << 3),
        })
    }
    fn configure(&mut self) -> Checked<()> {
        let c = self.controls()?;
        if let Some(route) = &mut self.route {
            route.configure(c);
        }
        Ok(())
    }
    fn live(&self) -> Option<&LiveRoute> {
        self.route.as_ref().filter(|r| r.active())
    }
    fn stop(&mut self, error: Value) {
        self.route.take();
        let _ = self
            .control
            .call(&json!({"op":"mic_stopped","error":error}));
    }
    fn apply_stop_command(&mut self, origin: &str, body: &Value) -> Checked<Value> {
        let plan = op(&mut self.control, json!({"op":"mic_plan","body":body,"origin":origin}))?;
        if plan["action"] == "stop" { self.stop(Value::Null); }
        Ok(plan["routeError"].clone())
    }
}
#[derive(Default)]
struct Devices {
    pair: Value,
    connected: String,
    checked: Option<Instant>,
    alias_error: Value,
}
pub(crate) struct Mixer {
    state: Mutex<State>,
    devices: Mutex<Devices>,
    profile: PathBuf,
    assets: PathBuf,
    supported: bool,
}
impl Mixer {
    pub(crate) fn new(profile_dir: &Path, assets: &Path, supported: bool) -> Self {
        let mut control = ControlState::default();
        for (kind, name) in [
            ("gain", "output-gains.json"),
            ("duck", "mixer-ducking.json"),
            ("mic", "microphone-input.json"),
        ] {
            let read = profile::load_profile(&profile_dir.join(name), |v| {
                valid_profile(kind, &Value::Object(v.clone()))
            });
            let bad = matches!(read,Err(e) if e!=2 && e!=3);
            let _ =
                control.call(&json!({"op":format!("{kind}_load"),"profile":read.ok(),"bad":bad}));
        }
        Self {
            state: Mutex::new(State {
                control,
                route: None,
            }),
            devices: Mutex::new(Devices::default()),
            profile: profile_dir.into(),
            assets: assets.into(),
            supported,
        }
    }
    pub(crate) fn watch(
        self: &Arc<Self>,
        stop: std::sync::mpsc::Receiver<()>,
    ) -> std::thread::JoinHandle<()> {
        let api = Arc::clone(self);
        std::thread::spawn(move || {
            while matches!(
                stop.recv_timeout(Duration::from_secs(2)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                let original = {
                    let mut s = api.state.lock().unwrap();
                    if s.route.is_none() {
                        continue;
                    }
                    s.control
                        .call(&json!({"op":"mic_state"}))
                        .unwrap_or_default()["activeId"]
                        .clone()
                };
                let rows = runtime::inputs(); // COM inventory may block. Fader updates continue.
                let mut s = api.state.lock().unwrap();
                if s.route.is_none()
                    || s.control
                        .call(&json!({"op":"mic_state"}))
                        .unwrap_or_default()["activeId"]
                        != original
                {
                    continue;
                }
                let running = s.live().is_some();
                let error = match rows {
                    Ok(rows) => s
                        .control
                        .call(&json!({"op":"mic_interrupted","rows":rows,"running":running}))
                        .unwrap_or(json!("route_interrupted")),
                    Err(_) => json!("route_interrupted"),
                };
                if !error.is_null() {
                    s.stop(error);
                }
            }
        })
    }
    pub(crate) fn shutdown(&self) {
        self.state.lock().unwrap().stop(Value::Null);
    }
    pub(crate) fn handle(
        &self,
        method: &str,
        path: &str,
        origin: &str,
        headers: &[(String, String)],
        payload: NativeBody<'_>,
    ) -> Reply {
        self.request(method, path, origin, headers, payload)
            .unwrap_or_else(|e| e)
    }
    fn request(
        &self,
        method: &str,
        path: &str,
        origin: &str,
        headers: &[(String, String)],
        payload: NativeBody<'_>,
    ) -> Checked<Reply> {
        if headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("cookie"))
        {
            return Err(failure(403, "cookie_free_request_required"));
        }
        let mut state = self.state.lock().unwrap();
        if path == "/mix/gains"
            && op(&mut state.control, json!({"op":"gain_get"}))?["readError"] == true
        {
            return Err(failure(503, "saved_settings_unreadable"));
        }
        let token = op(&mut state.control, json!({"op":"token","origin":origin}))?;
        let mut body = Value::Null;
        if method == "POST" {
            let provided: Vec<_> = headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("X-Lum-Mix-Token"))
                .map(|(_, v)| v)
                .collect();
            let parse = || -> Checked<Value> {
                let mime = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("Content-Type"))
                    .map_or("", |(_, v)| v.as_str());
                if !mime
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("application/json")
                {
                    return Err(if path == "/mix/mic" {
                        failure(415, "expected_json")
                    } else {
                        (415, Value::Null)
                    });
                }
                if payload.oversized {
                    let limit = payload_limit(path);
                    return Err((
                        413,
                        json!(format!(
                            "Maximum request body size {limit} exceeded, actual body size {}",
                            limit + 1
                        )),
                    ));
                }
                decode_charset(mime, payload.bytes)
                    .and_then(|bytes| profile::decode(&bytes).ok())
                    .map(Value::Object)
                    .ok_or_else(|| {
                        failure(
                            400,
                            if path == "/mix/preferences" {
                                "invalid_preferences"
                            } else {
                                "invalid_settings"
                            },
                        )
                    })
            };
            if path == "/mix/mic" {
                body = parse()?;
                if op(&mut state.control, json!({"op":"validate_mic","body":body}))? != true {
                    return Err(failure(400, "invalid_settings"));
                }
            }
            if op(
                &mut state.control,
                json!({"op":"authorize","origin":origin,"provided":provided,"body":body}),
            )?
            .is_null()
            {
                return Err(failure(
                    403,
                    if path == "/mix/preferences" {
                        "refresh_settings"
                    } else {
                        "refresh_mix_settings"
                    },
                ));
            }
            if path != "/mix/mic" {
                body = parse()?;
            }
        }
        match path {
            "/mix/gains" => {
                let mut write_failed = false;
                if method == "POST" {
                    if op(
                        &mut state.control,
                        json!({"op":"validate_gain","patch":body}),
                    )? != true
                    {
                        return Err(failure(400, "invalid_settings"));
                    }
                    let prop = op(&mut state.control, json!({"op":"gain_plan","patch":body}))?;
                    if let Some(v) = prop.as_object() {
                        if profile::save_profile(&self.profile.join("output-gains.json"), v)
                            .is_err()
                        {
                            write_failed = true;
                        } else {
                            op(
                                &mut state.control,
                                json!({"op":"gain_commit","profile":prop}),
                            )?;
                            state.configure()?;
                        }
                    }
                }
                let g = op(&mut state.control, json!({"op":"gain_get"}))?;
                let mut reply = object_merge(
                    g["gains"].clone(),
                    json!({"engineAvailable":true,"routeActive":state.live().is_some(),"revision":g["revision"],"token":token}),
                );
                if write_failed {
                    reply["error"] = json!("settings_not_saved");
                }
                Ok((if write_failed { 503 } else { 200 }, reply))
            }
            "/mix/duck" => {
                if method == "POST" {
                    let prop = op(&mut state.control, json!({"op":"duck_plan","body":body}))?;
                    if let Some(v) = prop.as_object() {
                        profile::save_profile(&self.profile.join("mixer-ducking.json"), v)
                            .map_err(|_| failure(503, "settings_not_saved"))?;
                        op(
                            &mut state.control,
                            json!({"op":"duck_commit","settings":prop}),
                        )?;
                        state.configure()?;
                    }
                } else if op(&mut state.control, json!({"op":"duck_get"}))?["bad"] == true {
                    return Err(failure(503, "settings_unavailable"));
                }
                let controls = state.controls()?;
                let route = state.live();
                let active = route.is_some();
                let metric = route.map(LiveRoute::metrics);
                let (source, edition) = route.map_or(("off", None), LiveRoute::source);
                let snapshot = op(
                    &mut state.control,
                    json!({"op":"duck_state","mixer":true,"active":active,
                    "present":metric.as_ref().is_some_and(|m|m.music!=0),"reduction":metric.as_ref().map_or(0.,|m|m.reduction),
                    "micMuted":controls.flags&1!=0||controls.mic==0.,"musicMuted":controls.flags&2!=0||controls.music==0.}),
                )?;
                Ok((
                    200,
                    object_merge(
                        snapshot,
                        json!({"token":token,"spotifyCapture":source,"spotifyEdition":edition}),
                    ),
                ))
            }
            "/mix/preferences" => {
                drop(state);
                let settings = if method == "POST" {
                    let obj = body
                        .as_object()
                        .ok_or_else(|| failure(400, "invalid_preferences"))?;
                    let patch = body["settings"]
                        .as_object()
                        .filter(|v| !v.is_empty() && profile::valid_studio_settings(v));
                    if patch.is_none()
                        || obj.keys().any(|k| k != "settings" && k != "migrate")
                        || body.get("migrate").is_some_and(|x| !x.is_boolean())
                    {
                        return Err(failure(400, "invalid_preferences"));
                    }
                    profile::studio_preferences(
                        &self.profile.join("studio-preferences.json"),
                        patch,
                        body["migrate"] == true,
                    )
                } else {
                    profile::studio_preferences(
                        &self.profile.join("studio-preferences.json"),
                        None,
                        false,
                    )
                }
                .map_err(|_| failure(503, "preferences_unavailable"))?;
                Ok((200, json!({"settings":settings,"token":token})))
            }
            "/mix/mic" => {
                drop(state);
                self.mic(origin, body, token)
            }
            _ => Err((404, Value::Null)),
        }
    }
    fn mic(&self, origin: &str, body: Value, token: Value) -> Checked<Reply> {
        let action = body["action"].as_str().unwrap_or("");
        let mut dev = self.devices.lock().unwrap();
        if matches!(action, "release" | "stop") {
            let mut s = self.state.lock().unwrap();
            let error = s.apply_stop_command(origin, &body)?;
            if !error.is_null() {
                return Self::snapshot(&mut s, &dev, &[], token, 409, error);
            }
        }
        let rows = runtime::inputs();
        if matches!(action, "start" | "setup")
            || dev
                .checked
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(30))
        {
            std::thread::scope(|scope| {
                let headsets = scope.spawn(setup_runtime::connected_headsets);
                dev.pair = setup_runtime::cable_pair(&self.assets);
                dev.connected = headsets.join().unwrap_or_default();
            });
            dev.checked = Some(Instant::now());
        }
        let rows = match rows {
            Ok(rows) => rows,
            Err(_) => {
                return Self::snapshot(
                    &mut self.state.lock().unwrap(),
                    &dev,
                    &[],
                    token,
                    503,
                    json!("route_unavailable"),
                );
            }
        };
        let mut s = self.state.lock().unwrap();
        let mut owner = None;
        if !matches!(action, "" | "release" | "stop") {
            let ready =
                !list(&dev.pair, "Render").is_empty() && !list(&dev.pair, "Capture").is_empty();
            let no_route = s.route.is_none();
            let plan = op(
                &mut s.control,
                json!({"op":"mic_plan","body":body,"origin":origin,"rows":rows,"connected":dev.connected,
                "routeAvailable":no_route,"cableReady":ready,"cableError":dev.pair["error"]}),
            )?;
            if !plan["routeError"].is_null() {
                return Self::snapshot(&mut s, &dev, &rows, token, 409, plan["routeError"].clone());
            }
            match plan["action"].as_str() {
                Some("select") => {
                    let saved = json!({"schema":1,"id":plan["id"]});
                    if profile::save_profile(
                        &self.profile.join("microphone-input.json"),
                        saved.as_object().unwrap(),
                    )
                    .is_err()
                    {
                        return Self::snapshot(
                            &mut s,
                            &dev,
                            &rows,
                            token,
                            503,
                            json!("settings_not_saved"),
                        );
                    }
                    op(&mut s.control, json!({"op":"mic_selected","id":plan["id"]}))?;
                }
                Some("start") => {
                    drop(s);
                    let alias = setup_runtime::ensure_alias(&self.assets);
                    dev.alias_error = alias["aliasError"].clone();
                    for key in ["captureName", "captureAlias"] {
                        dev.pair[key] = alias[key].clone();
                    }
                    let mut guard = self.state.lock().unwrap();
                    let controls = guard.controls()?;
                    drop(guard);
                    let route = LiveRoute::open(
                        &self.assets.join("libportaudio64bit.dll"),
                        plan["id"].as_str().unwrap_or(""),
                        &list(&dev.pair, "Render"),
                        controls,
                        self.supported,
                        lummix_media::lummix_audio_source,
                    );
                    s = self.state.lock().unwrap();
                    match route {
                        Ok(mut r) => {
                            r.configure(s.controls()?);
                            op(
                                &mut s.control,
                                json!({"op":"mic_started","id":plan["id"],"origin":origin,"token":token,"ownerToken":plan["ownerToken"]}),
                            )?;
                            s.route = Some(r);
                            owner = Some(plan["ownerToken"].clone());
                        }
                        Err(e) => return Self::snapshot(&mut s, &dev, &rows, token, 409, json!(e)),
                    }
                }
                _ => {}
            }
        }
        let (status, mut out) = Self::snapshot(&mut s, &dev, &rows, token, 200, Value::Null)?;
        if let Some(owner) = owner {
            out["ownerToken"] = owner;
        }
        Ok((status, out))
    }
    fn snapshot(
        s: &mut State,
        d: &Devices,
        rows: &[Value],
        token: Value,
        status: u16,
        error: Value,
    ) -> Checked<Reply> {
        let mut selected = op(
            &mut s.control,
            json!({"op":"mic_state","rows":rows,"connected":d.connected,"cableError":d.pair["error"]}),
        )?;
        let route = s.live();
        let (source, edition) = route.map_or(("off", None), LiveRoute::source);
        let latency = route.map(LiveRoute::latency);
        if route.is_none() {
            selected["activeId"] = Value::Null;
        }
        if !error.is_null() {
            selected["error"] = error;
        }
        let ms = |s: f64| (s * 10000.).round() / 10.;
        Ok((
            status,
            object_merge(
                selected,
                json!({"inputs":rows,"token":token,"routeActive":route.is_some(),
            "cableReady":!list(&d.pair,"Capture").is_empty()&&!list(&d.pair,"Render").is_empty(),
            "captureName":d.pair["captureName"],"captureAlias":d.pair["captureAlias"].as_str().unwrap_or("unavailable"),
            "aliasError":d.alias_error,"spotifyCapture":source,"spotifyEdition":edition,"micRms":route.map(|r|r.metrics().rms.clamp(0.,1.)),
            "latencyMode":latency.map(|(m,_)|m),"latencyMs":latency.map(|(_,l)|json!({"input":ms(l[0]),"output":ms(l[1])}))}),
            ),
        ))
    }
}
