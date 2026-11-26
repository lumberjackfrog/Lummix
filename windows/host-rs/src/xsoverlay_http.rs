use crate::local_http::{LoopbackClient, Response as UpstreamResponse};
use crate::private_http::{
    self, has_header, header_values, parse_body, single_header, Listener, Request,
};
use serde_json::{json, Value};
use std::{
    ffi::OsString, fs, net::Ipv4Addr, os::windows::ffi::OsStringExt, path::PathBuf, ptr::null_mut,
    slice, sync::Mutex,
};
use tiny_http::Method;

const HOST: &str = "127.0.0.1:41839";
const DESKTOP: &str = "http://127.0.0.1:41835";
const PANEL: &str = "http://127.0.0.1:41839";
const CONFIG: &str = "/lummix-addon";
const CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'self' file: http://localhost:* http://127.0.0.1:*";
const LIMIT: usize = 512_000;
const GAIN_KEYS: [&str; 3] = ["micMuted", "musicMuted", "musicGain"];

type Addon = extern "C" fn(action: u32, output: *mut u8, capacity: usize) -> usize;

struct Panel {
    root: PathBuf,
    studio: PathBuf,
    mixer_port: u16,
    qa: bool,
    mixer: Mutex<()>,
    addon: Mutex<Box<dyn Fn(u32) -> Option<Value> + Send>>,
    token: String,
}

fn panel_permitted(request: &Request) -> bool {
    let mut origins = header_values(request, "Origin");
    let origin_ok = match origins.next() {
        None => true,
        Some(PANEL) => origins.next().is_none(),
        _ => false,
    };
    origin_ok && has_header(request, "X-Lummix-Panel", "1")
}

fn send(request: Request, status: u16, body: Vec<u8>, mime: &str, cors: bool) {
    let mut headers = Vec::new();
    if mime.starts_with("text/html") {
        headers.push(("Content-Security-Policy", CSP));
    }
    if cors {
        headers.push(("Access-Control-Allow-Origin", DESKTOP));
        if status == 204 {
            headers.push(("Access-Control-Allow-Methods", "POST"));
            headers.push((
                "Access-Control-Allow-Headers",
                "Content-Type, X-Lummix-Addon-Token",
            ));
            headers.push((
                "Vary",
                "Origin, Access-Control-Request-Headers, Access-Control-Request-Method",
            ));
        } else {
            headers.push(("Vary", "Origin"));
        }
    }
    private_http::send(request, status, body, mime, &headers);
}

fn send_json(request: Request, status: u16, body: Value, cors: bool) {
    let body = if status == 204 {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    send(request, status, body, "application/json", cors);
}

fn reject(request: Request, status: u16, cors: bool) {
    send_json(request, status, json!({}), cors);
}

fn upstream(response: UpstreamResponse) -> Option<Value> {
    if response.status != 200 || response.body.len() > LIMIT {
        return None;
    }
    serde_json::from_slice(&response.body).ok()
}

fn valid_gains(value: &Value) -> bool {
    value.as_object().is_some_and(|body| {
        !body.is_empty()
            && body.iter().all(|(key, value)| match key.as_str() {
                "micMuted" | "musicMuted" => value.is_boolean(),
                "musicGain" => value.as_u64().is_some_and(|gain| gain <= 200),
                _ => false,
            })
    })
}

impl Panel {
    fn asset(&self, route: &str) -> Option<(PathBuf, &'static str)> {
        match route {
            "/wrist" => Some((
                self.root.join("xsoverlay-panel.html"),
                "text/html; charset=utf-8",
            )),
            "/controls-bridge.js" => {
                Some((self.studio.join("controls-bridge.js"), "text/javascript"))
            }
            "/controls.wasm" => Some((self.studio.join("controls.wasm"), "application/wasm")),
            _ => None,
        }
    }

    fn config(&self, request: Request, action: u32) {
        let result = self.addon.lock().ok().and_then(|callback| callback(action));
        let Some(Value::Object(mut response)) = result else {
            return reject(request, 503, true);
        };
        let expected = if action == 1 { "ready" } else { "installable" };
        let status =
            if action == 0 || response.get("state").and_then(Value::as_str) == Some(expected) {
                200
            } else {
                409
            };
        response.insert("token".into(), json!(self.token));
        send_json(request, status, Value::Object(response), true);
    }

    fn mixer_data(&self, step: bool, patch: Option<Value>) -> Option<Value> {
        let client = LoopbackClient::at_qa_address(self.qa).ok()?;
        if step {
            let response = upstream(
                client
                    .get(self.mixer_port, "/mix/preferences", true, LIMIT + 1)
                    .ok()?,
            )?;
            let snap = response
                .get("settings")
                .and_then(|settings| settings.get("snap"));
            return Some(json!({"step": match snap {
                None => 10,
                Some(snap) => snap.as_u64().filter(|value| matches!(value, 5 | 10 | 20)).unwrap_or(1),
            }}));
        }
        let _guard = self.mixer.lock().ok()?;
        let mut values = upstream(
            client
                .get(self.mixer_port, "/mix/gains", true, LIMIT + 1)
                .ok()?,
        )?;
        if let Some(patch) = patch {
            let token = values.get("token")?.as_str()?;
            values = upstream(
                client
                    .post_studio_json(
                        self.mixer_port,
                        "/mix/gains",
                        token,
                        patch.to_string().as_bytes(),
                        LIMIT + 1,
                    )
                    .ok()?,
            )?;
        }
        let mut public = serde_json::Map::new();
        for key in GAIN_KEYS {
            public.insert(key.into(), values.get(key)?.clone());
        }
        let public = Value::Object(public);
        valid_gains(&public).then_some(public)
    }

    fn controls(&self, request: Request, step: bool, patch: Option<Value>) {
        match self.mixer_data(step, patch) {
            Some(data) => send_json(request, 200, data, false),
            None => send_json(request, 503, json!({"error": "Mixer unavailable"}), false),
        }
    }

    fn handle(&self, mut request: Request) {
        if !has_header(&request, "Host", HOST) {
            return reject(request, 403, false);
        }
        let method = request.method.clone();
        let config = request.target.as_str() == CONFIG;
        let control = request.target.as_str() == "/controls";
        let desktop = has_header(&request, "Origin", DESKTOP);
        match method {
            Method::Get => {
                let route = request
                    .target
                    .split_once('?')
                    .map_or(request.target.as_str(), |(path, _)| path);
                if let Some((asset, mime)) = self.asset(route) {
                    return match fs::read(asset) {
                        Ok(bytes) => send(request, 200, bytes, mime, false),
                        Err(_) => reject(request, 404, false),
                    };
                }
                match route {
                    CONFIG if desktop => self.config(request, 0),
                    "/controls" | "/step" if panel_permitted(&request) => {
                        let step = route == "/step";
                        self.controls(request, step, None);
                    }
                    CONFIG | "/controls" | "/step" => reject(request, 403, false),
                    _ => reject(request, 404, false),
                }
            }
            Method::Options => {
                let fields = single_header(&request, "Access-Control-Request-Headers")
                    .unwrap_or("")
                    .split(',')
                    .map(|part| part.trim().to_ascii_lowercase())
                    .collect::<std::collections::HashSet<_>>();
                if config
                    && desktop
                    && has_header(&request, "Access-Control-Request-Method", "POST")
                    && fields.len() == 2
                    && fields.contains("content-type")
                    && fields.contains("x-lummix-addon-token")
                {
                    send_json(request, 204, Value::Null, true);
                } else {
                    reject(request, 403, false);
                }
            }
            Method::Post => {
                if !((config && desktop) || (control && panel_permitted(&request))) {
                    return reject(request, 403, false);
                }
                if config && !has_header(&request, "X-Lummix-Addon-Token", &self.token) {
                    return reject(request, 403, true);
                }
                let body = match parse_body(&mut request) {
                    Ok(body) => body,
                    Err(status) => return reject(request, status, config),
                };
                if config {
                    if body == json!({"action":"install"}) {
                        self.config(request, 1);
                    } else if body == json!({"action":"remove"}) {
                        self.config(request, 2);
                    } else {
                        reject(request, 400, true);
                    }
                } else if valid_gains(&body) {
                    self.controls(request, false, Some(body));
                } else {
                    reject(request, 400, false);
                }
            }
            _ => reject(request, 405, false),
        }
    }
}

pub fn open_native(
    root: PathBuf,
    bind: u16,
    mixer_port: u16,
    callback: impl Fn(u32) -> Option<Value> + Send + 'static,
    address: Ipv4Addr,
) -> Option<(Listener, u16)> {
    if !root.join("xsoverlay-panel.html").is_file() {
        return None;
    }
    let studio = if root.join("controls.wasm").is_file() {
        root.clone()
    } else {
        root.parent()?.join("studio")
    };
    let state = Panel {
        root,
        studio,
        mixer_port,
        qa: address.octets()[3] == 2,
        mixer: Mutex::new(()),
        addon: Mutex::new(Box::new(callback)),
        token: private_http::token()?,
    };
    private_http::listen(address, bind, move |request| state.handle(request))
}

#[no_mangle]
pub unsafe extern "C" fn lummix_wrist_start(
    root: *const u16,
    length: usize,
    bind: u16,
    mixer_port: u16,
    callback: Option<Addon>,
    qa: bool,
    port: *mut u16,
) -> *mut Listener {
    if root.is_null() || port.is_null() || length == 0 || length > 32_767 || mixer_port == 0 {
        return null_mut();
    }
    let Some(callback) = callback else {
        return null_mut();
    };
    let root = PathBuf::from(OsString::from_wide(slice::from_raw_parts(root, length)));
    let address = Ipv4Addr::new(127, 0, 0, if qa { 2 } else { 1 });
    let Some((listener, bound)) = open_native(
        root,
        bind,
        mixer_port,
        move |action| {
            let mut bytes = [0; 4096];
            let size = callback(action, bytes.as_mut_ptr(), bytes.len());
            serde_json::from_slice(bytes.get(..size)?).ok()
        },
        address,
    ) else {
        return null_mut();
    };
    port.write(bound);
    Box::into_raw(Box::new(listener))
}

#[no_mangle]
pub unsafe extern "C" fn lummix_wrist_stop(listener: *mut Listener) {
    if !listener.is_null() {
        drop(Box::from_raw(listener));
    }
}
