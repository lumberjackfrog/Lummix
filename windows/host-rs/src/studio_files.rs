const ASSETS: &str = include_str!("../../runtime/studio-assets.txt");

pub fn route(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or_default()
}

pub fn asset(route: &str) -> Option<(String, &'static str)> {
    let path = self::route(route);
    let name = if path == "/" {
        "index.html"
    } else {
        path.strip_prefix('/')?
    };
    let allowed = ASSETS.split_whitespace().find(|allowed| *allowed == name)?;
    let mime = match allowed.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html",
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("wasm") => "application/wasm",
        _ => "text/plain",
    };
    Some((allowed.replace("%20", " "), mime))
}
