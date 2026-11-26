use crate::private_http::{self, header_values, single_header, Listener, Request};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::Ipv4Addr,
    ptr::null_mut,
    slice,
    sync::atomic::{AtomicUsize, Ordering},
};
use tiny_http::Method;

const ALLOWED_ORIGINS: [&str; 2] = ["http://127.0.0.1:41835", "http://localhost:41835"];
const ALLOWED_HEADERS: &str =
    "Content-Type, X-Lum-Spotify-Token, X-Lum-Duck-Token, X-Lum-Mix-Token";
const WORKERS: usize = 12;
const LONG_POLLS: usize = 6;
const RESPONSE_LIMIT: usize = 2_000_000;

type Dispatch = extern "C" fn(
    dispatch: *const u8,
    dispatch_length: usize,
    body: *const u8,
    body_length: usize,
    output: *mut u8,
    capacity: usize,
    status: *mut u16,
) -> usize;

pub(crate) struct NativeBody<'a> {
    pub bytes: &'a [u8],
    pub oversized: bool,
}

pub(crate) type NativeDispatch = dyn Fn(&str, &str, &str, &str, &[(String, String)], NativeBody<'_>) -> (u16, Vec<u8>) + Send + Sync;

pub(crate) fn payload_limit(path: &str) -> usize {
    if path == "/mix/preferences" { 401_000 } else { 256 }
}
enum Backend {
    Legacy(Dispatch),
    Rust(Box<NativeDispatch>),
}

struct Media {
    host: String,
    routes: HashMap<String, Methods>,
    dispatch: Backend,
    poll_count: AtomicUsize,
}

#[derive(Clone, Copy, Default)]
struct Methods {
    get: bool,
    post: bool,
}

impl Methods {
    fn allows(self, method: &str) -> bool {
        match method {
            "GET" => self.get,
            "POST" => self.post,
            _ => false,
        }
    }

    fn list(self) -> &'static str {
        match (self.get, self.post) {
            (true, true) => "GET, POST",
            (true, false) => "GET",
            (false, true) => "POST",
            (false, false) => "",
        }
    }
}

struct PollSlot<'a>(&'a AtomicUsize);

impl<'a> PollSlot<'a> {
    fn acquire(count: &'a AtomicUsize) -> Option<Self> {
        count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < LONG_POLLS).then_some(active + 1)
            })
            .ok()?;
        Some(Self(count))
    }
}

impl Drop for PollSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn send(
    request: Request,
    status: u16,
    body: Vec<u8>,
    mime: &str,
    origin: Option<&str>,
    extra: &[(&str, &str)],
) {
    let mut headers = extra.to_vec();
    if let Some(origin) = origin {
        headers.push(("Access-Control-Allow-Origin", origin));
        headers.push(("Vary", "Origin"));
    }
    private_http::send(request, status, body, mime, &headers);
}

fn failure(request: Request, status: u16) {
    send(
        request,
        status,
        Vec::new(),
        "application/octet-stream",
        None,
        &[],
    );
}

impl Media {
    fn handle(&self, mut request: Request) {
        let origin = single_header(&request, "Origin").unwrap_or("");
        let origin = ALLOWED_ORIGINS
            .iter()
            .copied()
            .find(|value| *value == origin);
        let Some(origin) =
            origin.filter(|_| single_header(&request, "Host") == Some(self.host.as_str()))
        else {
            return send(
                request,
                403,
                br#"{"error":"unauthorized_origin"}"#.to_vec(),
                "application/json",
                None,
                &[],
            );
        };
        let (path, query) = request
            .target
            .split_once('?')
            .unwrap_or((&request.target, ""));
        let Some(path) = private_http::decode_path(path) else {
            return failure(request, 404);
        };
        let Some(&methods) = self.routes.get(&path) else {
            return failure(request, 404);
        };
        if request.method == Method::Options {
            let accepted = single_header(&request, "Access-Control-Request-Method")
                .is_some_and(|method| methods.allows(method));
            return send(
                request,
                if accepted { 204 } else { 405 },
                Vec::new(),
                "application/octet-stream",
                Some(origin),
                &[
                    ("Access-Control-Allow-Methods", methods.list()),
                    ("Access-Control-Allow-Headers", ALLOWED_HEADERS),
                ],
            );
        }
        if !methods.allows(request.method.as_str()) {
            return send(
                request,
                405,
                Vec::new(),
                "application/octet-stream",
                None,
                &[("Allow", methods.list())],
            );
        }
        let query = query.to_owned();
        let limit = payload_limit(&path);
        let (body, oversized) = if request.method == Method::Post {
            match private_http::read_payload(&mut request, limit) {
                Ok(body) => (body, false),
                Err(413) => (Vec::new(), true),
                Err(code) => return failure(request, code),
            }
        } else {
            (Vec::new(), false)
        };
        let _poll = if path == "/spotify/changes" {
            let Some(slot) = PollSlot::acquire(&self.poll_count) else {
                return send(
                    request,
                    503,
                    Vec::new(),
                    "application/octet-stream",
                    Some(origin),
                    &[],
                );
            };
            Some(slot)
        } else {
            None
        };
        let (status,response) = match &self.dispatch {
            Backend::Rust(dispatch) => {
                use std::io::Read;
                let decode = |reader: &mut dyn Read| {
                    let mut inflated = Vec::new();
                    reader.take(limit as u64 + 1).read_to_end(&mut inflated)
                        .map(|_| inflated).unwrap_or_default()
                };
                let encoding = header_values(&request, "Content-Encoding").next().unwrap_or("");
                let body = if encoding.eq_ignore_ascii_case("gzip") {
                    decode(&mut flate2::read::GzDecoder::new(&body[..]))
                } else if encoding.eq_ignore_ascii_case("deflate") {
                    decode(&mut flate2::read::ZlibDecoder::new(&body[..]))
                } else { body };
                let oversized = oversized || body.len() > limit;
                if oversized && !path.starts_with("/mix/") {
                    (413, b"Request body too large".to_vec())
                } else {
                    dispatch(request.method.as_str(), &path, origin, &query, request.headers(),
                             NativeBody { bytes: &body, oversized })
                }
            },
            Backend::Legacy(dispatch) => {
                let meta = json!({"path":path,"query":query,"method":request.method.as_str(),
                                 "headers":request.headers(),"oversized":oversized}).to_string();
                let mut response = Vec::with_capacity(RESPONSE_LIMIT);
                let mut status = 0;
                let size = dispatch(meta.as_ptr(),meta.len(),body.as_ptr(),body.len(),
                                    response.as_mut_ptr(),response.capacity(),&mut status);
                if size > RESPONSE_LIMIT || !(100..=599).contains(&status) {
                    (503, Vec::new())
                } else {
                    // This compatibility callback initializes exactly the reported prefix.
                    unsafe {response.set_len(size);}
                    (status,response)
                }
            }
        };
        let size=response.len();
        let mime = if status == 413 {
            "text/plain; charset=utf-8"
        } else if size == 0 {
            "application/octet-stream"
        } else {
            "application/json"
        };
        send(
            request,
            status,
            response,
            mime,
            (status != 413).then_some(origin),
            &[],
        );
    }
}

fn open(config: Value, port: u16, qa: bool, dispatch: Backend) -> Option<(Listener, u16)> {
    let host = config["host"].as_str()?.to_owned();
    let mut routes: HashMap<String, Methods> = HashMap::new();
    for row in config["routes"].as_array()? {
        let path = row.get(0)?.as_str()?.to_owned();
        let methods = routes.entry(path).or_default();
        match row.get(1)?.as_str()? {
            "GET" => methods.get = true,
            "POST" => methods.post = true,
            _ => return None,
        }
    }
    let state = Media {
        host,
        routes,
        dispatch,
        poll_count: AtomicUsize::new(0),
    };
    private_http::listen_workers(
        Ipv4Addr::new(127, 0, 0, if qa { 2 } else { 1 }),
        port,
        WORKERS,
        move |request| state.handle(request),
    )
}

pub(crate) fn listen_native(routes: &[(&str,&str)], qa: bool, dispatch: Box<NativeDispatch>) -> Option<Listener> {
    let methods: Vec<_> = routes.iter().map(|(method,path)| [*path,*method]).collect();
    let config = json!({"host":"127.0.0.1:41838","routes":methods});
    open(config,41838,qa,Backend::Rust(dispatch)).map(|(listener,_)|listener)
}

#[no_mangle]
pub unsafe extern "C" fn lummix_media_start(
    config: *const u8,
    length: usize,
    port: u16,
    qa: bool,
    dispatch: Option<Dispatch>,
    bound: *mut u16,
) -> *mut Listener {
    if config.is_null() || length == 0 || length > 32_768 || bound.is_null() {
        return null_mut();
    }
    let Some(dispatch) = dispatch else {
        return null_mut();
    };
    let Ok(config) = serde_json::from_slice(slice::from_raw_parts(config, length)) else {
        return null_mut();
    };
    let Some((listener, port)) = open(config, port, qa, Backend::Legacy(dispatch)) else {
        return null_mut();
    };
    bound.write(port);
    Box::into_raw(Box::new(listener))
}

#[no_mangle]
pub unsafe extern "C" fn lummix_media_stop(listener: *mut Listener) {
    if !listener.is_null() {
        drop(Box::from_raw(listener));
    }
}
