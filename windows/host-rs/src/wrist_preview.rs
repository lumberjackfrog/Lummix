use crate::private_http::{
    self, has_header, header_values, parse_body, single_header, Listener, Request,
};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    net::Ipv4Addr,
    path::{Component, Path, PathBuf},
    ptr::null_mut,
    slice,
    sync::Mutex,
    time::{Instant, UNIX_EPOCH},
};
use tiny_http::Method;

const HOST: &str = "127.0.0.1:41836";
const DESKTOP: &str = "http://127.0.0.1:41835";
const WRIST: &str = "/vendor/_UI/Default/Wrist.html";
const SETUP: &str = "/setup-cable";
const CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; font-src 'self' data:; img-src 'self' data: blob:; connect-src 'self'; frame-ancestors http://127.0.0.1:41835 file:; base-uri 'none'; form-action 'none'";
const MISSING: &str = r#"<!doctype html><meta charset="utf-8"><style>html{color-scheme:dark;font:16px system-ui}body{padding:1em}</style><p>Wrist preview requires XSOverlay on this PC.</p>"#;

type Setup = extern "C" fn(action: u32, output: *mut u8, capacity: usize) -> usize;

struct Preview {
    root: PathBuf,
    installed: Option<PathBuf>,
    config: PathBuf,
    setup: Setup,
    token: Mutex<Token>,
}

struct Token {
    value: String,
    issued: Instant,
    used: bool,
}

impl Token {
    fn new() -> Option<Self> {
        Some(Self {
            value: private_http::token()?,
            issued: Instant::now(),
            used: false,
        })
    }

    fn valid(&self) -> bool {
        self.issued.elapsed().as_secs() <= 300
    }

    fn matches(&self, offered: &str) -> bool {
        self.valid()
            && self.value.len() == offered.len()
            && self
                .value
                .bytes()
                .zip(offered.bytes())
                .fold(0, |difference, (expected, seen)| {
                    difference | (expected ^ seen)
                })
                == 0
    }
}

fn send(request: Request, status: u16, body: Vec<u8>, mime: &str, extra: &[(&str, &str)]) {
    let mut headers = vec![("Content-Security-Policy", CSP)];
    headers.extend_from_slice(extra);
    private_http::send(request, status, body, mime, &headers);
}

fn asset(request: Request, body: Vec<u8>, mime: &str) {
    send(request, 200, body, mime, &[]);
}

fn reject(request: Request, status: u16) {
    send(request, status, b"{}".to_vec(), "application/json", &[]);
}

fn cors_json(request: Request, status: u16, data: Value) {
    send(
        request,
        status,
        data.to_string().into_bytes(),
        "application/json",
        &[("Access-Control-Allow-Origin", DESKTOP), ("Vary", "Origin")],
    );
}

fn mime(file: &Path) -> Option<&'static str> {
    Some(
        match file.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "js" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "html" => "text/html; charset=utf-8",
            "png" => "image/png",
            "jpg" => "image/jpeg",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            "otf" => "font/otf",
            _ => return None,
        },
    )
}

fn insert_before(target: &mut Vec<u8>, marker: &[u8], data: &[u8], offset: usize) {
    if let Some(position) = target
        .windows(marker.len())
        .position(|candidate| candidate == marker)
    {
        target.splice(position + offset..position + offset, data.iter().copied());
    }
}

fn mtime(path: PathBuf) -> Option<u128> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_nanos())
}

fn read_vendor(root: &Path, route: &str) -> Option<(Vec<u8>, &'static str)> {
    let name = route.strip_prefix("/vendor/")?;
    if name.contains(['\\', ':', '\0']) {
        return None;
    }
    let relative = Path::new(name);
    if !relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let path = root.join(relative).canonicalize().ok()?;
    if !path.starts_with(root) || !path.is_file() {
        return None;
    }
    let kind = mime(&path)?;
    Some((fs::read(path).ok()?, kind))
}

impl Preview {
    fn callback(&self, action: u32) -> Option<Value> {
        let mut output = vec![0; 512_000];
        let size = (self.setup)(action, output.as_mut_ptr(), output.len());
        serde_json::from_slice(output.get(..size)?).ok()
    }

    fn setup(&self, mut request: Request, post: bool) {
        if !has_header(&request, "Origin", DESKTOP) {
            return reject(request, 403);
        }
        let Ok(mut token) = self.token.lock() else {
            return reject(request, 503);
        };
        let mut action = 0;
        if post {
            if !single_header(&request, "X-Lum-Setup-Token")
                .is_some_and(|header| token.matches(header))
            {
                return reject(request, 403);
            }
            if header_values(&request, "Transfer-Encoding")
                .next()
                .is_some()
            {
                return reject(request, 415);
            }
            if !single_header(&request, "Content-Length")
                .and_then(|value| value.parse::<usize>().ok())
                .is_some_and(|size| (2..=32).contains(&size))
            {
                return reject(request, 400);
            }
            match parse_body(&mut request) {
                Ok(value) if value.as_object().is_some_and(|body| body.is_empty()) => {}
                Err(status) => return reject(request, status),
                _ => return reject(request, 400),
            }
            if !token.used {
                action = 1;
                token.used = true;
            }
        } else if !token.valid() || token.used {
            let Some(next) = Token::new() else {
                return reject(request, 503);
            };
            *token = next;
        }
        let Some(Value::Object(mut body)) = self.callback(action) else {
            return reject(request, 503);
        };
        let busy = body
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| matches!(state, "checking" | "preparing" | "awaiting-windows"));
        body.insert("token".into(), json!(token.value));
        cors_json(
            request,
            if post && busy { 202 } else { 200 },
            Value::Object(body),
        );
    }

    fn vendor(&self, request: Request, route: &str) {
        let asset_data = self
            .installed
            .as_ref()
            .and_then(|root| read_vendor(root, route));
        match asset_data {
            Some((mut bytes, kind)) => {
                if route == WRIST {
                    let Ok(tile) = fs::read(self.root.join("wrist-tile.html")) else {
                        return reject(request, 404);
                    };
                    insert_before(
                        &mut bytes,
                        b"<head>",
                        b"<script src=\"/wrist-fixture.js\"></script>",
                        6,
                    );
                    insert_before(&mut bytes, b"</body>", &tile, 0);
                }
                asset(request, bytes, kind);
            }
            None => reject(request, 404),
        }
    }

    fn fixture(&self, request: Request) {
        let prefs = fs::read(&self.config)
            .ok()
            .and_then(|data| serde_json::from_slice::<Value>(&data).ok());
        let mut snapshot = serde_json::Map::new();
        if let Some(prefs) = prefs {
            for key in [
                "UseDarkTheme",
                "AccentColor",
                "MediaThemeing",
                "DefaultShowBatteryPercentage",
                "BatteryFontScale",
            ] {
                snapshot.insert(key.into(), prefs[key].clone());
            }
        }
        let Ok(script) = fs::read(self.root.join("wrist-fixture.js")) else {
            return reject(request, 404);
        };
        let mut prefix =
            format!("const savedSettings = {};\n", Value::Object(snapshot)).into_bytes();
        prefix.extend_from_slice(&script);
        asset(request, prefix, "text/javascript; charset=utf-8");
    }

    fn handle(&self, request: Request) {
        if !has_header(&request, "Host", HOST) {
            return reject(request, 403);
        }
        if request.method == Method::Options && request.target == SETUP {
            let headers = single_header(&request, "Access-Control-Request-Headers")
                .unwrap_or("")
                .split(',')
                .map(|header| header.trim().to_ascii_lowercase())
                .collect::<HashSet<_>>();
            if has_header(&request, "Origin", DESKTOP)
                && has_header(&request, "Access-Control-Request-Method", "POST")
                && headers.len() == 2
                && headers.contains("content-type")
                && headers.contains("x-lum-setup-token")
            {
                return send(
                    request,
                    204,
                    Vec::new(),
                    "application/json",
                    &[
                        ("Access-Control-Allow-Origin", DESKTOP),
                        ("Access-Control-Allow-Methods", "POST"),
                        (
                            "Access-Control-Allow-Headers",
                            "Content-Type, X-Lum-Setup-Token",
                        ),
                        (
                            "Vary",
                            "Origin, Access-Control-Request-Method, Access-Control-Request-Headers",
                        ),
                    ],
                );
            }
            return reject(request, 403);
        }
        if request.method == Method::Post && request.target == SETUP {
            return self.setup(request, true);
        }
        if request.method != Method::Get {
            return reject(request, 404);
        }
        let raw = request
            .target
            .split_once('?')
            .map_or(request.target.as_str(), |(path, _)| path);
        let Some(route) = private_http::decode_path(raw) else {
            return reject(request, 404);
        };
        match route.as_str() {
            "/lummix-health" => asset(request, json!({
                "service":"lummix-wrist", "ready":true, "vendorAvailable":self.installed.is_some(), "cableRestartSupport":true
            }).to_string().into_bytes(), "application/json"),
            "/preview" if self.installed.is_some() => send(request, 302, Vec::new(), "text/html", &[("Location", WRIST)]),
            "/preview" => asset(request, MISSING.as_bytes().to_vec(), "text/html; charset=utf-8"),
            "/revision" => {
                let a = mtime(self.root.join("wrist-tile.html"));
                let b = mtime(self.root.join("wrist-fixture.js"));
                match a.zip(b) {
                    Some((a, b)) => asset(request, format!("{a}-{b}").into_bytes(), "text/plain; charset=utf-8"),
                    None => reject(request, 404),
                }
            }
            "/wrist-fixture.js" => self.fixture(request),
            "/setup-status" => self.setup(request, false),
            "/detect-devices" if !has_header(&request, "Origin", DESKTOP) => reject(request, 403),
            "/detect-devices" => match self.callback(2) {
                Some(data) => cors_json(request, 200, data),
                None => reject(request, 503),
            },
            _ => self.vendor(request, &route),
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lummix_preview_start(
    paths: *const u8,
    length: usize,
    bind: u16,
    qa: bool,
    callback: Option<Setup>,
    port: *mut u16,
) -> *mut Listener {
    fn open(paths: Value, bind: u16, qa: bool, callback: Setup) -> Option<(Listener, u16)> {
        let installed = paths["vendor"]
            .as_str()
            .and_then(|path| fs::canonicalize(path).ok())
            .filter(|path| path.join("_UI/Default/Wrist.html").is_file());
        let state = Preview {
            root: PathBuf::from(paths["root"].as_str()?),
            installed,
            config: PathBuf::from(paths["config"].as_str()?),
            setup: callback,
            token: Mutex::new(Token::new()?),
        };
        private_http::listen(
            Ipv4Addr::new(127, 0, 0, if qa { 2 } else { 1 }),
            bind,
            move |request| state.handle(request),
        )
    }
    if paths.is_null() || port.is_null() || length == 0 || length > 384_000 {
        return null_mut();
    }
    let Some(callback) = callback else {
        return null_mut();
    };
    let Ok(paths) = serde_json::from_slice(slice::from_raw_parts(paths, length)) else {
        return null_mut();
    };
    let Some((listener, bound)) = open(paths, bind, qa, callback) else {
        return null_mut();
    };
    port.write(bound);
    Box::into_raw(Box::new(listener))
}
