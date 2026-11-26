use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::{desktop, diagnostics, profile, release_feed};
use release_feed::Failure;

#[derive(Clone, Copy)]
enum UpdateAction {
    Check,
    Download,
    Install,
}

struct UpdateState {
    phase: &'static str,
    message: &'static str,
    active: bool,
    last_request: Option<Instant>,
    release: Option<Value>,
}

struct SpotifyState {
    phase: String,
    active: bool,
}

pub(crate) struct Jobs {
    profile: PathBuf,
    updates: PathBuf,
    version: &'static str,
    cancelled: AtomicBool,
    window: AtomicUsize,
    update: Mutex<UpdateState>,
    spotify: Mutex<SpotifyState>,
}

impl Jobs {
    pub(crate) fn new(profile: &Path, version: &'static str) -> Arc<Self> {
        let updates = profile.join("Updates");
        let _ = lummix_media::api::spotify_setup_cleanup(profile);
        if release_feed::cleanup(&updates).is_err() {
            diagnostics::record(profile, version, "update_storage");
        }
        Arc::new(Self {
            profile: profile.to_owned(),
            updates,
            version,
            cancelled: AtomicBool::new(false),
            window: AtomicUsize::new(0),
            update: Mutex::new(UpdateState {
                phase: "idle",
                message: "",
                active: false,
                last_request: None,
                release: None,
            }),
            spotify: Mutex::new(SpotifyState {
                phase: "idle".to_owned(),
                active: false,
            }),
        })
    }

    pub(crate) fn attach(&self, window: usize) {
        self.window.store(window, Ordering::Release);
    }

    pub(crate) fn stop(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.window.store(0, Ordering::Release);
    }

    pub(crate) fn stopping(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn changed(&self, event: u32) {
        let window = self.window.load(Ordering::Acquire);
        if window != 0 && !self.stopping() {
            desktop::lummix_desktop_notify(window as _, event);
        }
    }

    pub(crate) fn update_state(&self) -> Value {
        let (phase, message, release) = self
            .update
            .lock()
            .map(|state| {
                let release = state
                    .release
                    .as_ref()
                    .and_then(|value| value["tag"].as_str())
                    .and_then(|tag| tag.strip_prefix('v'))
                    .map(str::to_owned);
                (state.phase, state.message, release)
            })
            .unwrap_or(("error", "The update state is unavailable.", None));
        json!({"type":"lummix-update","phase":phase,"current":self.version,"version":release,"message":message})
    }

    pub(crate) fn update_command(self: &Arc<Self>, command: &str) {
        let (action, selected) = {
            let Ok(mut state) = self.update.lock() else {
                return;
            };
            if state.active || self.stopping() {
                return;
            }
            if command == "auto" && release_feed::checked_recently(&self.updates, self.version) {
                state.phase = "current";
                return;
            }
            let (action, phase) = match command {
                "check" | "auto"
                    if matches!(state.phase, "idle" | "current" | "error" | "available") =>
                {
                    if state
                        .last_request
                        .is_some_and(|last| last.elapsed() < Duration::from_secs(3))
                    {
                        return;
                    }
                    state.last_request = Some(Instant::now());
                    (UpdateAction::Check, "checking")
                }
                "download" if state.phase == "available" => (UpdateAction::Download, "downloading"),
                "install" if state.phase == "ready" => (UpdateAction::Install, "installing"),
                _ => return,
            };
            state.active = true;
            state.phase = phase;
            state.message = "";
            (action, state.release.clone())
        };
        self.changed(desktop::UPDATE);
        let jobs = self.clone();
        if thread::Builder::new()
            .name("Lummix update".to_owned())
            .spawn(move || jobs.run_update(action, selected))
            .is_err()
        {
            self.finish_update(
                "error",
                "Windows could not start the update worker.",
                None,
                Some("update_storage"),
            );
        }
    }

    fn run_update(&self, action: UpdateAction, mut release: Option<Value>) {
        let (phase, message, event) = match action {
            UpdateAction::Check => match release_feed::latest_version(self.version) {
                Ok(result) => {
                    release = result;
                    if release.is_some() {
                        ("available", "", Some("update_available"))
                    } else {
                        let _ = release_feed::write_check(&self.updates, self.version);
                        ("current", "", None)
                    }
                }
                Err(error) => failure(action, error),
            },
            UpdateAction::Download | UpdateAction::Install => {
                let result = release
                    .as_ref()
                    .ok_or(Failure::Invalid)
                    .and_then(|document| match action {
                        UpdateAction::Download => {
                            release_feed::download_release(&self.updates, document, || {
                                !self.stopping()
                            })
                        }
                        UpdateAction::Install => {
                            release_feed::install_release(&self.updates, document, || {
                                !self.stopping()
                            })
                        }
                        _ => Err(Failure::Invalid),
                    });
                match (action, result) {
                    (UpdateAction::Download, Ok(())) => ("ready", "", Some("update_verified")),
                    (UpdateAction::Install, Ok(())) => ("launched", "", Some("update_launched")),
                    (_, Err(error)) => failure(action, error),
                    _ => ("error", "", None),
                }
            }
        };
        self.finish_update(phase, message, release, event);
    }

    fn finish_update(
        &self,
        phase: &'static str,
        message: &'static str,
        release: Option<Value>,
        event: Option<&str>,
    ) {
        if let Ok(mut state) = self.update.lock() {
            state.phase = phase;
            state.message = message;
            state.release = release;
            state.active = false;
        }
        if !self.stopping() {
            if let Some(event) = event {
                diagnostics::record(&self.profile, self.version, event);
            }
            self.changed(desktop::UPDATE);
        }
    }

    pub(crate) fn spotify_state(&self) -> Value {
        let phase = self
            .spotify
            .lock()
            .map(|state| state.phase.clone())
            .unwrap_or_else(|_| "failed".to_owned());
        json!({"type":"lummix-spotify-setup","phase":phase})
    }

    pub(crate) fn spotify_begin(self: &Arc<Self>) {
        {
            let Ok(mut state) = self.spotify.lock() else {
                return;
            };
            if state.active || self.stopping() {
                return;
            }
            state.active = true;
            state.phase = "checking".to_owned();
        }
        self.changed(desktop::SPOTIFY);
        let jobs = self.clone();
        if thread::Builder::new()
            .name("Lummix Spotify setup".to_owned())
            .spawn(move || {
                let saved = profile::load_profile(
                    &jobs.profile.join("spotify-locations.json"),
                    |locations| {
                        locations.iter().all(|(key, value)| {
                            (key == "desktop" || key == "store")
                                && value
                                    .as_str()
                                    .is_some_and(|path| Path::new(path).is_absolute())
                        })
                    },
                )
                .ok();
                let preferred = saved
                    .as_ref()
                    .and_then(|locations| locations.get("desktop"))
                    .and_then(Value::as_str)
                    .map(Path::new);
                let phase = lummix_media::api::spotify_setup(&jobs.profile, preferred, |phase| {
                    if !phase.is_empty() {
                        if let Ok(mut state) = jobs.spotify.lock() {
                            state.phase = phase.to_owned();
                        }
                        jobs.changed(desktop::SPOTIFY);
                    }
                    !jobs.stopping()
                });
                jobs.finish_spotify(phase);
            })
            .is_err()
        {
            self.finish_spotify(Err("Spotify worker unavailable".into()));
        }
    }

    fn finish_spotify(&self, phase: Result<String, String>) {
        if phase.is_err() && !self.stopping() {
            diagnostics::record(&self.profile, self.version, "spotify_setup_failed");
        }
        if let Ok(mut state) = self.spotify.lock() {
            state.active = false;
            state.phase = phase.unwrap_or_else(|_| "failed".to_owned());
        }
        self.changed(desktop::SPOTIFY);
    }
}

fn failure(
    action: UpdateAction,
    error: Failure,
) -> (&'static str, &'static str, Option<&'static str>) {
    match error {
        Failure::Invalid | Failure::Current => ("error", "Release verification failed. Check again when the publisher has corrected the download.", Some("update_invalid")),
        Failure::Network => ("error", "Couldn’t reach GitHub. Try again when you’re online.", Some("update_network")),
        Failure::Cancelled if matches!(action, UpdateAction::Install) => ("ready", "Stop your mix before installing. If the mixer is still starting, try again in a moment.", Some("update_busy")),
        Failure::Storage if matches!(action, UpdateAction::Install) => ("ready", "Windows could not open the installer. Check Windows Security or try again.", Some("update_blocked")),
        _ => ("error", "Couldn’t save the update. Check your free disk space and try again.", Some("update_storage")),
    }
}
