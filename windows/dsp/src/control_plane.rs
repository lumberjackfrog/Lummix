use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    ffi::{c_char, CStr, CString},
    ptr,
    sync::Mutex,
    time::{Duration, Instant},
};

const TOKEN_TTL: Duration = Duration::from_secs(600);
const BRANDS: [&str; 6] = ["pico", "oculus", "quest", "vive", "index", "pimax"];

#[link(name = "bcrypt")]
extern "system" {
    fn BCryptGenRandom(
        algorithm: *const std::ffi::c_void,
        buffer: *mut u8,
        count: u32,
        flags: u32,
    ) -> i32;
}

fn token() -> Result<String, &'static str> {
    let mut random = [0u8; 32];
    let code = unsafe { BCryptGenRandom(ptr::null(), random.as_mut_ptr(), 32, 2) };
    if code < 0 {
        return Err("Could not generate a mixer token");
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut token = String::with_capacity(43);
    for bytes in random.chunks(3) {
        token.push(alphabet[(bytes[0] >> 2) as usize] as char);
        let second = bytes.get(1).copied().unwrap_or_default();
        token.push(alphabet[((bytes[0] & 3) << 4 | second >> 4) as usize] as char);
        if bytes.len() > 1 {
            let third = bytes.get(2).copied().unwrap_or_default();
            token.push(alphabet[((second & 15) << 2 | third >> 6) as usize] as char);
            if bytes.len() == 3 {
                token.push(alphabet[(third & 63) as usize] as char);
            }
        }
    }
    Ok(token)
}

fn secret_matches(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

fn gain_defaults() -> Value {
    json!({"micGain":100,"musicGain":100,"micMuted":false,"musicMuted":false})
}

fn duck_defaults() -> Value {
    json!({"enabled":true,"depthDb":12})
}

fn valid_gain(patch: &Value) -> bool {
    patch.as_object().is_some_and(|fields| {
        !fields.is_empty()
            && fields.iter().all(|(key, value)| match key.as_str() {
                "micGain" | "musicGain" => value
                    .as_f64()
                    .is_some_and(|value| (0.0..=200.0).contains(&value)),
                "micMuted" | "musicMuted" => value.is_boolean(),
                _ => false,
            })
    })
}

fn valid_duck(patch: &Value) -> bool {
    patch.as_object().is_some_and(|fields| {
        !fields.is_empty()
            && fields.iter().all(|(key, value)| match key.as_str() {
                "enabled" => value.is_boolean(),
                "depthDb" => value
                    .as_i64()
                    .is_some_and(|value| (1..=24).contains(&value)),
                _ => false,
            })
    })
}

pub fn valid_profile(kind: &str, data: &Value) -> bool {
    match kind {
        "gain" => data["revision"].as_u64().is_some() && valid_gain(&data["gains"]),
        "mic" => {
            (data["schema"].as_f64() == Some(1.0) || data["schema"] == true)
                && data
                    .get("id")
                    .is_some_and(|id| id.is_null() || id.is_string())
        }
        "duck" => valid_duck(data),
        _ => false,
    }
}

fn valid_mic(body: &Value) -> bool {
    let Some(fields) = body.as_object() else {
        return false;
    };
    match body["action"].as_str().unwrap_or_default() {
        action @ ("select" | "setup" | "start") => {
            fields.len() == 2
                && fields.get("id").is_some_and(|id| {
                    (id.is_null() && action == "select")
                        || id
                            .as_str()
                            .is_some_and(|id| !id.is_empty() && id.chars().take(129).count() <= 128)
                })
        }
        "stop" => fields.len() == 1,
        "release" => fields.len() == 2 && body["ownerToken"].is_string(),
        _ => false,
    }
}

fn has_input(rows: &[Value], selected: &str) -> bool {
    rows.iter().any(|row| row["id"].as_str() == Some(selected))
}

fn recommend<'a>(rows: &'a [Value], saved: Option<&'a str>, connected: &str) -> Option<&'a str> {
    if let Some(saved) = saved.filter(|id| !id.is_empty() && has_input(rows, id)) {
        return Some(saved);
    }
    let mut selected = None;
    for row in rows {
        let name = row["name"].as_str().unwrap_or_default().to_lowercase();
        let live_headset = row["headset"] == true
            && BRANDS
                .iter()
                .any(|brand| name.contains(brand) && connected.contains(brand));
        let kind = row["kind"].as_str().unwrap_or_default();
        if kind == "headsetBridge" && !live_headset {
            continue;
        }
        let score = (
            live_headset,
            kind == "headset",
            row["defaultCommunications"] == true,
            row["defaultInput"] == true,
        );
        if let Some(id) = row["id"].as_str() {
            // Python max retains the first row on a tie.
            if selected
                .as_ref()
                .is_none_or(|(previous, _)| score > *previous)
            {
                selected = Some((score, id));
            }
        }
    }
    selected.map(|(_, id)| id)
}

fn select<'a>(
    rows: &'a [Value],
    saved: Option<&'a str>,
    recommendation: Option<&'a str>,
) -> Option<&'a str> {
    match saved {
        Some(saved) if has_input(rows, saved) => Some(saved),
        Some(saved) if !saved.is_empty() => None,
        _ => recommendation,
    }
}

fn merged(original: &Value, patch: &Value) -> Value {
    let mut fields = original.as_object().cloned().unwrap_or_default();
    if let Some(patch) = patch.as_object() {
        fields.extend(patch.clone());
    }
    Value::Object(fields)
}

fn gain_equal(left: &Value, right: &Value) -> bool {
    for key in ["micGain", "musicGain"] {
        if left[key].as_f64() != right[key].as_f64() {
            return false;
        }
    }
    left["micMuted"] == right["micMuted"] && left["musicMuted"] == right["musicMuted"]
}

struct Owner {
    origin: String,
    source_token: String,
    token: String,
}

struct Mic {
    saved: Option<String>,
    active: Option<String>,
    owner: Option<Owner>,
    error: Value,
    usable: bool,
}

pub struct ControlState {
    revision: u64,
    gains: Value,
    read_error: bool,
    tokens: HashMap<String, (String, Instant)>,
    mic: Mic,
    duck: Value,
    duck_configured: bool,
    duck_bad: bool,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            revision: 0,
            gains: gain_defaults(),
            read_error: false,
            tokens: HashMap::new(),
            mic: Mic {
                saved: None,
                active: None,
                owner: None,
                error: Value::Null,
                usable: true,
            },
            duck: duck_defaults(),
            duck_configured: false,
            duck_bad: false,
        }
    }
}

struct Control(Mutex<ControlState>);

fn rows(input: &Value) -> &[Value] {
    input["rows"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn route_error(error: &str) -> Value {
    json!({"routeError": error})
}

impl ControlState {
    fn create_token(&mut self, origin: &str) -> Result<Value, &'static str> {
        let now = Instant::now();
        if let Some((entry, until)) = self.tokens.get(origin) {
            if *until > now {
                return Ok(json!(entry));
            }
        }
        let current = token()?;
        self.tokens
            .insert(origin.to_owned(), (current.clone(), now + TOKEN_TTL));
        Ok(json!(current))
    }

    fn authorize(&self, input: &Value) -> Option<String> {
        let origin = input["origin"].as_str()?;
        let provided = input["provided"].as_array()?;
        let [provided] = provided.as_slice() else {
            return None;
        };
        let provided = provided.as_str().filter(|provided| provided.is_ascii())?;
        let active = self
            .tokens
            .get(origin)
            .filter(|(_, expires)| *expires > Instant::now());
        if let Some((current, _)) = active.filter(|(current, _)| secret_matches(provided, current))
        {
            return Some(current.clone());
        }
        if input["body"]["action"] == "release" {
            if let Some(owner) = &self.mic.owner {
                if owner.origin == origin
                    && secret_matches(&owner.source_token, provided)
                    && input["body"]["ownerToken"]
                        .as_str()
                        .is_some_and(|supplied| secret_matches(&owner.token, supplied))
                {
                    return Some(provided.to_owned());
                }
            }
        }
        None
    }

    fn gain_state(&self) -> Value {
        json!({"gains": self.gains,"revision": self.revision,"readError": self.read_error})
    }

    fn load_gain(&mut self, input: &Value) -> Value {
        let data = &input["profile"];
        self.read_error = input["bad"] == true;
        if let Some(revision) = data["revision"]
            .as_u64()
            .filter(|_| valid_profile("gain", data))
        {
            self.gains = merged(&gain_defaults(), &data["gains"]);
            self.revision = revision;
        }
        self.gain_state()
    }

    fn gain_plan(&self, input: &Value) -> Result<Value, &'static str> {
        if !valid_gain(&input["patch"]) {
            return Err("invalid_settings");
        }
        let updated = merged(&self.gains, &input["patch"]);
        if gain_equal(&updated, &self.gains) {
            return Ok(Value::Null);
        }
        let revision = self.revision.checked_add(1).ok_or("settings_not_saved")?;
        Ok(json!({"revision":revision,"gains":updated}))
    }

    fn load_mic(&mut self, input: &Value) {
        self.mic.usable = input["bad"] != true;
        self.mic.error = if self.mic.usable {
            Value::Null
        } else {
            json!("saved_settings_unreadable")
        };
        self.mic.saved = input["profile"]["id"].as_str().map(str::to_owned);
    }

    fn mic_state(&self, input: &Value) -> Value {
        let saved = self.mic.saved.as_deref();
        let recommended = recommend(
            rows(input),
            saved,
            input["connected"].as_str().unwrap_or_default(),
        );
        let selected = select(rows(input), saved, recommended);
        let error = if self.mic.error.is_null()
            && input["cableError"].is_null()
            && saved.is_some_and(|saved| !saved.is_empty())
            && selected.is_none()
        {
            json!("selected_input_unavailable")
        } else if !self.mic.error.is_null() {
            self.mic.error.clone()
        } else {
            input["cableError"].clone()
        };
        json!({"mode":if saved.is_some_and(|saved| !saved.is_empty()) { "manual" } else { "auto" },
            "savedId":saved,"recommendedId":recommended,"selectedId":selected,"activeId":self.mic.active,"error":error})
    }

    fn mic_plan(&self, input: &Value) -> Result<Value, &'static str> {
        let body = &input["body"];
        if !valid_mic(body) {
            return Err("invalid_settings");
        }
        let action = body["action"].as_str().unwrap_or_default();
        if matches!(action, "release" | "stop") {
            if self.mic.active.is_none() {
                return Ok(Value::Null);
            }
            if action == "release"
                && !self.mic.owner.as_ref().is_some_and(|owner| {
                    input["origin"].as_str() == Some(&owner.origin)
                        && body["ownerToken"]
                            .as_str()
                            .is_some_and(|token| secret_matches(token, &owner.token))
                })
            {
                return Ok(route_error("route_owner_changed"));
            }
            return Ok(json!({"action":"stop"}));
        }
        if !self.mic.usable {
            return Ok(route_error("saved_settings_unreadable"));
        }
        if action == "select" {
            if self.mic.active.is_some() {
                return Ok(route_error("stop_before_select"));
            }
            let selected = body["id"].as_str();
            if selected.is_some_and(|id| !has_input(rows(input), id)) {
                return Ok(route_error("selected_input_unavailable"));
            }
            return Ok(json!({"action":"select","id": selected}));
        }
        if self.mic.active.is_some() {
            return Ok(route_error("route_already_active"));
        }
        if input["routeAvailable"] != true || self.read_error {
            return Ok(route_error("route_unavailable"));
        }
        let state = self.mic_state(input);
        let Some(selected) = state["selectedId"]
            .as_str()
            .filter(|value| !value.is_empty())
        else {
            return Ok(route_error(
                if self.mic.saved.as_ref().is_some_and(|id| !id.is_empty()) {
                    "selected_input_unavailable"
                } else {
                    "no_input"
                },
            ));
        };
        if body["id"].as_str() != Some(selected) {
            return Ok(route_error("selection_changed"));
        }
        if input["cableReady"] != true {
            return Ok(route_error(
                input["cableError"].as_str().unwrap_or("cable_unavailable"),
            ));
        }
        Ok(json!({"action":"start","id":selected,"ownerToken":token()?}))
    }

    fn mic_started(&mut self, input: &Value) -> Result<(), &'static str> {
        let id = input["id"].as_str().ok_or("invalid_settings")?;
        let origin = input["origin"].as_str().ok_or("invalid_settings")?;
        let source_token = input["token"].as_str().ok_or("invalid_settings")?;
        let owner = input["ownerToken"].as_str().ok_or("invalid_settings")?;
        self.mic.active = Some(id.to_owned());
        self.mic.error = Value::Null;
        self.mic.owner = Some(Owner {
            origin: origin.to_owned(),
            source_token: source_token.to_owned(),
            token: owner.to_owned(),
        });
        Ok(())
    }

    fn load_duck(&mut self, input: &Value) {
        self.duck_bad = input["bad"] == true;
        self.duck = duck_defaults();
        self.duck_configured = false;
        if valid_duck(&input["profile"]) {
            self.duck = merged(&self.duck, &input["profile"]);
            self.duck_configured = true;
        }
    }

    fn duck_plan(&self, input: &Value) -> Result<Value, &'static str> {
        let mut patch = input["body"]
            .as_object()
            .cloned()
            .ok_or("invalid_settings")?;
        let migrate = patch
            .remove("migrate")
            .unwrap_or(json!(false))
            .as_bool()
            .ok_or("invalid_settings")?;
        let patch = Value::Object(patch);
        if !valid_duck(&patch) {
            return Err("invalid_settings");
        }
        if self.duck_bad {
            return Err("settings_unavailable");
        }
        let updated = merged(&self.duck, &patch);
        if (migrate && self.duck_configured) || (self.duck_configured && updated == self.duck) {
            return Ok(Value::Null);
        }
        Ok(updated)
    }

    fn duck_state(&self, params: &Value) -> Value {
        let present = params["present"] == true;
        let mut reduction = params["reduction"].as_f64().unwrap_or_default();
        let state = if params["mixer"] != true {
            "unavailable"
        } else if self.duck["enabled"] != true {
            "off"
        } else if params["active"] != true {
            "waiting_mic"
        } else if !present {
            "waiting_music"
        } else if params["micMuted"] == true {
            "mic_muted"
        } else if params["musicMuted"] == true {
            "music_muted"
        } else if reduction >= 0.1 {
            "ducking"
        } else {
            "ready"
        };
        reduction = (reduction * 100.0).round_ties_even() / 100.0;
        let mut result = self.duck.as_object().cloned().unwrap_or_else(Map::new);
        result.extend(Map::from_iter([
            ("configured".to_owned(), json!(self.duck_configured)),
            ("state".to_owned(), json!(state)),
            ("source".to_owned(), json!(present.then_some("mixer_music"))),
            ("reductionDb".to_owned(), json!(reduction)),
        ]));
        Value::Object(result)
    }

    pub fn call(&mut self, input: &Value) -> Result<Value, &'static str> {
        match input["op"].as_str().unwrap_or_default() {
            "validate_profile" => Ok(json!(valid_profile(
                input["kind"].as_str().unwrap_or_default(),
                &input["profile"]
            ))),
            "validate_gain" => Ok(json!(valid_gain(&input["patch"]))),
            "validate_mic" => Ok(json!(valid_mic(&input["body"]))),

            "token" => self.create_token(input["origin"].as_str().ok_or("invalid_settings")?),
            "authorize" => Ok(json!(self.authorize(input))),
            "expire_token" => {
                self.tokens
                    .remove(input["origin"].as_str().unwrap_or_default());
                Ok(Value::Null)
            }
            "gain_load" => Ok(self.load_gain(input)),
            "gain_get" => Ok(self.gain_state()),
            "gain_plan" => self.gain_plan(input),
            "gain_commit" => {
                let data = &input["profile"];
                if !valid_profile("gain", data) {
                    return Err("invalid_settings");
                }
                self.gains = data["gains"].clone();
                self.revision = data["revision"].as_u64().unwrap();
                Ok(Value::Null)
            }
            "mic_load" => {
                self.load_mic(input);
                Ok(Value::Null)
            }
            "mic_state" => Ok(self.mic_state(input)),
            "mic_active" => Ok(json!(self.mic.active)),
            "mic_plan" => self.mic_plan(input),
            "mic_selected" => {
                self.mic.saved = input["id"].as_str().map(str::to_owned);
                self.mic.error = Value::Null;
                Ok(Value::Null)
            }
            "mic_started" => {
                self.mic_started(input)?;
                Ok(Value::Null)
            }
            "mic_stopped" => {
                self.mic.active = None;
                self.mic.owner = None;
                self.mic.error = input["error"].clone();
                Ok(Value::Null)
            }
            "mic_interrupted" => {
                let error = self.mic.active.as_deref().and_then(|id| {
                    if !has_input(rows(input), id) {
                        Some("input_disconnected")
                    } else if input["running"] != true {
                        Some("route_interrupted")
                    } else {
                        None
                    }
                });
                Ok(json!(error))
            }
            "duck_load" => {
                self.load_duck(input);
                Ok(Value::Null)
            }
            "duck_get" => Ok(json!({"settings":self.duck,"bad":self.duck_bad})),
            "duck_plan" => self.duck_plan(input),
            "duck_commit" => {
                if !valid_duck(&input["settings"]) {
                    return Err("invalid_settings");
                }
                self.duck = input["settings"].clone();
                self.duck_configured = true;
                Ok(Value::Null)
            }
            "duck_state" => Ok(self.duck_state(input)),
            _ => Err("invalid_settings"),
        }
    }
}

#[no_mangle]
extern "C" fn lummix_control_new() -> *mut Control {
    Box::into_raw(Box::new(Control(Mutex::new(ControlState::default()))))
}

#[no_mangle]
unsafe extern "C" fn lummix_control_free(control: *mut Control) {
    if !control.is_null() {
        drop(unsafe { Box::from_raw(control) });
    }
}

#[no_mangle]
unsafe extern "C" fn lummix_control_call(
    control: *const Control,
    request: *const c_char,
) -> *mut c_char {
    if control.is_null() || request.is_null() {
        return ptr::null_mut();
    }
    let request: Result<Value, _> =
        serde_json::from_slice(unsafe { CStr::from_ptr(request).to_bytes() });
    let response = match request {
        Ok(input) => {
            let control = unsafe { &*control };
            match control
                .0
                .lock()
                .map_err(|_| "control_unavailable")
                .and_then(|mut state| state.call(&input))
            {
                Ok(value) => json!({"value":value}),
                Err(error) => json!({"error":error}),
            }
        }
        Err(_) => json!({"error":"invalid_settings"}),
    };
    CString::new(response.to_string()).unwrap().into_raw()
}

#[no_mangle]
unsafe extern "C" fn lummix_control_free_json(response: *mut c_char) {
    if !response.is_null() {
        drop(unsafe { CString::from_raw(response) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_a_fractional_gain_across_json_and_a_restart() {
        let request: Value =
            serde_json::from_str(r#"{"patch":{"micGain":127.88535969157675}}"#).unwrap();
        let proposed = ControlState::default().gain_plan(&request).unwrap();
        let mut restored = ControlState::default();
        let serialized = serde_json::to_string(&proposed).unwrap();
        let profile: Value = serde_json::from_str(&serialized).unwrap();
        let result = restored.load_gain(&json!({"profile":profile}));
        assert_eq!(
            result["gains"]["micGain"].as_f64().unwrap().to_bits(),
            127.88535969157675_f64.to_bits()
        );
    }

    #[test]
    fn expired_token_only_releases_matching_owner_from_its_origin() {
        let mut state = ControlState::default();
        let origin = "http://127.0.0.1:41835";
        let original = "A".repeat(43);
        let owner = "B".repeat(43);
        state
            .mic_started(
                &json!({"id":"fixture","origin":origin,"token":original,"ownerToken":owner}),
            )
            .unwrap();
        state.tokens.insert(
            origin.to_owned(),
            (original.clone(), Instant::now() - Duration::from_secs(1)),
        );
        let command = |from: &str, action: &str, nonce: &str| json!({"origin":from,"provided":[original],"body":{"action":action,"ownerToken":nonce}});
        assert!(state
            .authorize(&command(origin, "release", &owner))
            .is_some());
        assert!(state.authorize(&command(origin, "stop", &owner)).is_none());
        assert!(state
            .authorize(&command(origin, "release", "wrong"))
            .is_none());
        assert!(state
            .authorize(&command("http://localhost:41835", "release", &owner))
            .is_none());
    }
}
