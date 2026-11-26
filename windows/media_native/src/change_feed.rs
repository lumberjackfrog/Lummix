use super::{
    discard_manager, encode_response, manager, spotify_source, Apartment, Manager, MediaEvent,
    Session,
};
use serde_json::{json, Value};
use std::{
    ffi::{c_char, CStr},
    ptr,
    sync::{mpsc, Arc, Condvar, Mutex},
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
    closed: bool,
}

#[derive(Default)]
struct Signal {
    status: Mutex<Status>,
    changed: Condvar,
}

fn publish(state: &Signal, notify: Option<MediaEvent>, events: bool) {
    let sequence = {
        let mut state = state.status.lock().unwrap();
        if state.closed {
            return;
        }
        state.events = events;
        state.sequence += 1;
        state.sequence
    };
    state.changed.notify_all();
    if let Some(notify) = notify {
        notify(sequence);
    }
}

fn listen(
    binding: &mut Binding,
    tx: &mpsc::Sender<Event>,
    rx: &mpsc::Receiver<Event>,
    state: &Signal,
    notify: Option<MediaEvent>,
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
    state: Arc<Signal>,
    notify: Option<MediaEvent>,
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

pub(super) struct Feed {
    tx: mpsc::Sender<Event>,
    thread: Mutex<Option<JoinHandle<()>>>,
    prefix: String,
    state: Arc<Signal>,
}

impl Feed {
    pub(super) fn new(prefix: String, notify: Option<MediaEvent>) -> std::io::Result<Self> {
        let (host, rx) = mpsc::channel();
        let tx = host.clone();
        let (ready, waited) = mpsc::channel();
        let state = Arc::new(Signal::default());
        let worker = state.clone();
        let thread = thread::Builder::new()
            .name("Lummix media events".into())
            .spawn(move || run(tx, rx, worker, notify, ready))?;
        let _ = waited.recv_timeout(Duration::from_secs(3));
        Ok(Self {
            tx: host,
            thread: Mutex::new(Some(thread)),
            prefix,
            state,
        })
    }

    fn revision(&self, sequence: u64) -> String {
        format!("{}.{}", self.prefix, sequence)
    }

    pub(super) fn read(&self, since: Option<&str>) -> Value {
        let mut status = self.state.status.lock().unwrap();
        if status.events && !status.closed && since == Some(self.revision(status.sequence).as_str())
        {
            let previous = status.sequence;
            (status, _) = self
                .state
                .changed
                .wait_timeout_while(status, Duration::from_secs(22), |status| {
                    status.sequence == previous && !status.closed
                })
                .unwrap();
        }
        json!({"revision":self.revision(status.sequence),"events":status.events})
    }

    pub(super) fn shutdown(&self) {
        {
            let mut status = self.state.status.lock().unwrap();
            if !status.closed {
                status.closed = true;
                status.events = false;
                status.sequence += 1;
            }
        }
        self.state.changed.notify_all();
        let _ = self.tx.send(Event::Stop);
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[no_mangle]
unsafe extern "C" fn lummix_feed_start(
    prefix: *const c_char,
    notify: Option<MediaEvent>,
) -> *mut Feed {
    if prefix.is_null() || notify.is_none() {
        return ptr::null_mut();
    }
    let Ok(prefix) = (unsafe { CStr::from_ptr(prefix) }).to_str() else {
        return ptr::null_mut();
    };
    Feed::new(prefix.to_owned(), notify)
        .map_or(ptr::null_mut(), |feed| Box::into_raw(Box::new(feed)))
}

#[no_mangle]
unsafe extern "C" fn lummix_feed_read(feed: *const Feed) -> *mut c_char {
    let Some(feed) = (unsafe { feed.as_ref() }) else {
        return ptr::null_mut();
    };
    encode_response(Ok(feed.read(None)))
}

#[no_mangle]
unsafe extern "C" fn lummix_feed_close(feed: *mut Feed) {
    if !feed.is_null() {
        drop(unsafe { Box::from_raw(feed) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_wakes_all_long_poll_waiters() {
        let (tx, _rx) = mpsc::channel();
        let feed = Feed {
            tx,
            thread: Mutex::new(None),
            prefix: "fixture".into(),
            state: Arc::new(Signal::default()),
        };
        publish(&feed.state, None, true);
        assert_eq!(
            feed.read(None),
            json!({"events":true,"revision":"fixture.1"})
        );
        thread::scope(|scope| {
            let waits = (0..4)
                .map(|_| scope.spawn(|| feed.read(Some("fixture.1"))))
                .collect::<Vec<_>>();
            thread::sleep(Duration::from_millis(20));
            let before = Instant::now();
            feed.shutdown();
            for waiter in waits {
                assert_eq!(
                    waiter.join().unwrap(),
                    json!({"events":false,"revision":"fixture.2"})
                );
            }
            assert!(before.elapsed() < Duration::from_secs(1));
        });
    }
}
