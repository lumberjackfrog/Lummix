#[cfg(windows)]
mod api_runtime;
#[cfg(windows)]
mod cable_setup;
#[cfg(windows)]
mod desktop;
#[cfg(windows)]
mod desktop_app;
#[cfg(windows)]
mod desktop_jobs;
#[cfg(windows)]
mod diagnostics;
#[cfg(windows)]
mod embedded_studio;
#[cfg(windows)]
mod endpoint_alias;
#[cfg(windows)]
mod lifecycle;
#[cfg(windows)]
mod local_http;
#[cfg(windows)]
mod media_http;
#[cfg(windows)]
mod mixer_api;
#[cfg(windows)]
mod private_http;
#[cfg(windows)]
mod profile;
#[cfg(windows)]
mod release_feed;
#[cfg(windows)]
mod setup_runtime;
#[cfg(windows)]
mod sign_in;
#[cfg(windows)]
mod studio;
mod studio_files;
#[cfg(windows)]
mod studio_response;
#[cfg(windows)]
mod update;
#[cfg(windows)]
mod vr_catalog;
#[cfg(windows)]
mod vr_discovery;
#[cfg(windows)]
mod vr_openvr;
#[cfg(windows)]
mod webview;
#[cfg(windows)]
mod window;
#[cfg(windows)]
mod wrist_preview;
#[cfg(windows)]
mod xsoverlay;
#[cfg(windows)]
mod xsoverlay_http;

#[cfg(windows)]
pub fn run() -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    desktop_app::entry(&executable, &args)
}
