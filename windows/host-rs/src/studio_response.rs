use std::path::Path;

use crate::studio_files::{asset, route};

pub const HOST: &str = "127.0.0.1:41835";
const HEALTH: &[u8] = br#"{"service":"lummix-studio","ready":true}"#;

pub struct Response {
    pub status: u16,
    pub mime: &'static str,
    pub body: Vec<u8>,
}

pub fn response(get: bool, valid_host: bool, target: &str, root: &Path) -> Response {
    let (status, mime, bytes): (u16, &'static str, &[u8]) = if !get {
        (501, "text/plain", b"Unsupported method")
    } else if !valid_host {
        (403, "text/plain", b"Forbidden")
    } else if route(target) == "/lummix-health" {
        (200, "application/json", HEALTH)
    } else {
        if let Some((name, mime)) = asset(target) {
            if let Ok(body) = std::fs::read(root.join(name)) {
                return Response {
                    status: 200,
                    mime,
                    body,
                };
            }
        }
        (404, "text/plain", b"Not Found")
    };
    Response {
        status,
        mime,
        body: bytes.to_vec(),
    }
}
