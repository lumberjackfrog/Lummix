use std::fs;
use std::io::{self, Cursor};
use std::path::Path;
use std::thread;

use tiny_http::{Header, Method, Request, Response, Server};

pub const HOST: &str = "127.0.0.1:41835";
const ASSETS: &str = include_str!("../../runtime/studio-assets.txt");
const HEALTH: &[u8] = br#"{"service":"lummix-studio","ready":true}"#;

fn asset(route: &str) -> Option<String> {
    let path = route.split(['?', '#']).next()?;
    let name = if path == "/" {
        "index.html"
    } else {
        path.strip_prefix('/')?
    };
    ASSETS
        .split_whitespace()
        .find(|allowed| *allowed == name)
        .map(|allowed| allowed.replace("%20", " "))
}

fn valid_host(headers: &[Header]) -> bool {
    let mut hosts = headers.iter().filter(|header| header.field.equiv("Host"));
    hosts
        .next()
        .is_some_and(|header| header.value.as_str() == HOST)
        && hosts.next().is_none()
}

fn reply(status: u16, mime: &'static str, data: Vec<u8>) -> Response<Cursor<Vec<u8>>> {
    let headers = [
        ("Content-Type", mime),
        ("X-Content-Type-Options", "nosniff"),
        ("Cache-Control", "no-store"),
    ]
    .map(|(name, value)| Header::from_bytes(name, value).expect("valid static HTTP header"));
    let length = data.len();
    Response::new(
        status.into(),
        headers.to_vec(),
        Cursor::new(data),
        Some(length),
        None,
    )
    .with_chunked_threshold(usize::MAX)
}

fn response(request: &Request, root: &Path) -> Response<Cursor<Vec<u8>>> {
    if request.method() != &Method::Get {
        return reply(501, "text/plain", b"Unsupported method".to_vec());
    }
    if !valid_host(request.headers()) {
        return reply(403, "text/plain", b"Forbidden".to_vec());
    }
    if request.url().split(['?', '#']).next() == Some("/lummix-health") {
        return reply(200, "application/json", HEALTH.to_vec());
    }
    if let Some(name) = asset(request.url()) {
        if let Ok(data) = fs::read(root.join(&name)) {
            let mime = match Path::new(&name).extension().and_then(|ext| ext.to_str()) {
                Some("html") => "text/html",
                Some("css") => "text/css",
                Some("js") => "text/javascript",
                Some("wasm") => "application/wasm",
                _ => "text/plain",
            };
            return reply(200, mime, data);
        }
    }
    reply(404, "text/plain", b"Not Found".to_vec())
}

pub fn serve(server: Server, root: &Path) -> io::Result<()> {
    thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                let _ = serve_requests(&server, root);
            });
        }
        serve_requests(&server, root)
    })
}

fn serve_requests(server: &Server, root: &Path) -> io::Result<()> {
    loop {
        let request = server.recv()?;
        let response = response(&request, root);
        let _ = request.respond(response);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn studio_routes_cover_encoded_space_and_only_allowed_entries() {
        assert_eq!(asset("/").as_deref(), Some("index.html"));
        assert_eq!(
            asset("/Mixer%20preview.html?v=1").as_deref(),
            Some("Mixer preview.html")
        );
        for path in [
            "../index.html",
            "/../index.html",
            "//index.html",
            "/%2e%2e/index.html",
            "/index%2ehtml",
            "/spotify-volume.js",
        ] {
            assert!(asset(path).is_none(), "{path}");
        }
    }

    #[test]
    fn host_must_be_exactly_one_canonical_value() {
        let host = Header::from_bytes("hOsT", HOST).unwrap();
        assert!(valid_host(&[host.clone()]));
        assert!(!valid_host(&[host.clone(), host]));
        assert!(!valid_host(&[]));
        assert!(!valid_host(&[Header::from_bytes(
            "Host",
            "127.0.0.2:41835"
        )
        .unwrap()]));
    }
}
