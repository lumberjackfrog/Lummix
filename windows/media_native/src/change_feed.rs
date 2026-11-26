use super::{
    discard_manager, encode_response, manager, spotify_source, Apartment, Manager, MediaEvent,
    Session,
};
use serde_json::json;
use std::{
    ffi::{c_char, CStr},
    ptr,
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::{
    core::Result,
    Foundation::{EventRegistrationToken, TypedEventHandler},
};

enum Event {
    Session,
    Inventory,
    Stop,
}

struct SessionBinding {
    target: Session,
    media: EventRegistrationToken,
    playback: EventRegistrationToken,
}

impl Drop for SessionBinding {
    fn drop(&mut self) {
        let _ = self.target.RemoveMediaPropertiesChanged(self.media);
        let _ = self.target.RemovePlaybackInfoChanged(self.playback);
    }
}

struct Binding {
    manager: Manager,
    token: EventRegistrationToken,
    sessions: Vec<SessionBinding>,
}

impl Drop for Binding {
    fn drop(&mut self) {
        self.sessions.clear();
        let _ = self.manager.RemoveSessionsChanged(self.token);
    }
}

fn bind_sessions(manager: &Manager, tx: &mpsc::Sender<Event>) -> Result<Vec<SessionBinding>> {
    let targets = manager.GetSessions()?;
    let mut sessions = Vec::new();
    for index in 0..targets.Size()? {
        let target = targets.GetAt(index)?;
        if !spotify_source(&target.SourceAppUserModelId()?.to_string()) {
            continue;
        }
        let on_media = tx.clone();
        let media = target.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
            let _ = on_media.send(Event::Session);
            Ok(())
        }))?;
        let on_playback = tx.clone();
        let playback = target.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
            let _ = on_playback.send(Event::Session);
            Ok(())
        }));
        match playback {
            Ok(playback) => sessions.push(SessionBinding {
                target,
                media,
                playback,
            }),
            Err(error) => {
                let _ = target.RemoveMediaPropertiesChanged(media);
                return Err(error);
            }
        }
    }
    Ok(sessions)
}

fn connect(tx: &mpsc::Sender<Event>) -> Result<Binding> {
    let manager = manager()?;
    let on_change = tx.clone();
    let token = manager
        .SessionsChanged(&TypedEventHandler::new(move |_, _| {
            let _ = on_change.send(Event::Inventory);
            Ok(())
        }))
        .inspect_err(|_| discard_manager(&manager))?;
    let mut binding = Binding {
        manager,
        token,
        sessions: Vec::new(),
    };
    binding.sessions =
        bind_sessions(&binding.manager, tx).inspect_err(|_| discard_manager(&binding.manager))?;
    Ok(binding)
}

#[derive(Default)]
struct Status {
    sequence: u64,
    events: bool,
}

fn publish(state: &Mutex<Status>, notify: MediaEvent, events: bool) {
    let sequence = {
        let mut state = state.lock().unwrap();
        state.events = events;
        state.sequence += 1;
        state.sequence
    };
    notify(sequence);
}

fn listen(
    binding: &mut Binding,
    tx: &mpsc::Sender<Event>,
    rx: &mpsc::Receiver<Event>,
    state: &Mutex<Status>,
    notify: MediaEvent,
) -> bool {
    loop {
        let mut update = match rx.recv() {
            Ok(Event::Session) => false,
            Ok(Event::Inventory) => true,
            _ => return false,
        };
        let deadline = Instant::now() + Duration::from_millis(55);
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Event::Inventory) => update = true,
                Ok(Event::Session) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                _ => return false,
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        if update {
            match bind_sessions(&binding.manager, tx) {
                Ok(next) if binding.sessions.is_empty() && next.is_empty() => continue,
                Ok(next) => binding.sessions = next,
                Err(_) => {
                    discard_manager(&binding.manager);
                    publish(state, notify, false);
                    return true;
                }
            }
        }
        publish(state, notify, true);
    }
}

fn run(
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    state: Arc<Mutex<Status>>,
    notify: MediaEvent,
    ready: mpsc::Sender<()>,
) {
    let Ok(_apartment) = Apartment::enter() else {
        return;
    };
    loop {
        let mut binding = connect(&tx).ok();
        if binding.is_some() {
            publish(&state, notify, true);
        }
        let _ = ready.send(());
        if let Some(binding) = binding.as_mut() {
            if !listen(binding, &tx, &rx, &state, notify) {
                return;
            }
        }
        drop(binding);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Event::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                _ if Instant::now() >= deadline => break,
                _ => {}
            }
        }
    }
}

struct Feed {
    tx: mpsc::Sender<Event>,
    thread: Option<JoinHandle<()>>,
    prefix: String,
    state: Arc<Mutex<Status>>,
}

impl Drop for Feed {
    fn drop(&mut self) {
        let _ = self.tx.send(Event::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[no_mangle]
unsafe extern "C" fn lummix_feed_start(
    prefix: *const c_char,
    notify: Option<MediaEvent>,
) -> *mut Feed {
    let Some(notify) = notify else {
        return ptr::null_mut();
    };
    if prefix.is_null() {
        return ptr::null_mut();
    }
    let Ok(prefix) = (unsafe { CStr::from_ptr(prefix) }).to_str() else {
        return ptr::null_mut();
    };
    let (host, rx) = mpsc::channel();
    let tx = host.clone();
    let (ready, waited) = mpsc::channel();
    let status = Arc::new(Mutex::new(Status::default()));
    let state = status.clone();
    let Ok(thread) = thread::Builder::new()
        .name("Lummix media events".into())
        .spawn(move || run(tx, rx, state, notify, ready))
    else {
        return ptr::null_mut();
    };
    let _ = waited.recv_timeout(Duration::from_secs(3));
    Box::into_raw(Box::new(Feed {
        tx: host,
        thread: Some(thread),
        prefix: prefix.to_owned(),
        state: status,
    }))
}

#[no_mangle]
unsafe extern "C" fn lummix_feed_read(feed: *const Feed) -> *mut c_char {
    let Some(feed) = (unsafe { feed.as_ref() }) else {
        return ptr::null_mut();
    };
    let state = feed.state.lock().unwrap();
    encode_response(Ok(
        json!({"revision":format!("{}.{}",feed.prefix,state.sequence),"events":state.events}),
    ))
}

#[no_mangle]
unsafe extern "C" fn lummix_feed_close(feed: *mut Feed) {
    if !feed.is_null() {
        drop(unsafe { Box::from_raw(feed) });
    }
}
