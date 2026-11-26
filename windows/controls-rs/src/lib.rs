use std::cell::RefCell;

// Each browser control session has its own WebAssembly instance.
const FIELDS: usize = 4;
const ABSENT: i32 = -1;
type Values = [i32; FIELDS];

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Request {
    None,
    Read,
    Write,
}

struct ControlQueue {
    committed: Values,
    pending: Values,
    writing: Values,
    request: Request,
    online: bool,
}

impl ControlQueue {
    const fn new() -> Self {
        Self {
            committed: [ABSENT; FIELDS],
            pending: [ABSENT; FIELDS],
            writing: [ABSENT; FIELDS],
            request: Request::None,
            online: false,
        }
    }

    fn has_pending(&self) -> bool {
        self.pending.iter().any(|&value| value != ABSENT)
    }

    fn next(&mut self, can_write: bool, poll: bool) -> Request {
        if self.request != Request::None {
            return Request::None;
        }
        self.request = if self.has_pending() && self.online && can_write {
            self.writing = std::mem::replace(&mut self.pending, [ABSENT; FIELDS]);
            Request::Write
        } else if self.has_pending() || poll {
            Request::Read
        } else {
            Request::None
        };
        self.request
    }

    fn success(&mut self, values: Values) {
        for (committed, value) in self.committed.iter_mut().zip(values) {
            if value != ABSENT {
                *committed = value;
            }
        }
        self.online = true;
        self.finish();
    }

    fn failure(&mut self, retain: bool) {
        if retain {
            for (pending, sent) in self.pending.iter_mut().zip(self.writing) {
                if *pending == ABSENT {
                    *pending = sent;
                }
            }
        } else {
            self.pending = [ABSENT; FIELDS];
        }
        self.online = false;
        self.finish();
    }

    fn finish(&mut self) {
        self.writing = [ABSENT; FIELDS];
        self.request = Request::None;
    }

    fn shown(&self, field: usize) -> i32 {
        [
            self.pending[field],
            self.writing[field],
            self.committed[field],
        ]
        .into_iter()
        .find(|&value| value != ABSENT)
        .unwrap_or(ABSENT)
    }
}

// Gain fields are microphone gain, music gain, microphone mute and music mute.
thread_local! {
    static GAINS: RefCell<ControlQueue> = const { RefCell::new(ControlQueue::new()) };
}

#[no_mangle]
pub extern "C" fn gain_change(field: usize, value: i32) {
    GAINS.with_borrow_mut(|gains| gains.pending[field] = value);
}

#[no_mangle]
pub extern "C" fn gain_shown(field: usize) -> i32 {
    GAINS.with_borrow(|gains| gains.shown(field))
}

#[no_mangle]
pub extern "C" fn gain_next(can_write: bool, poll: bool) -> i32 {
    GAINS.with_borrow_mut(|gains| gains.next(can_write, poll) as i32)
}

#[no_mangle]
pub extern "C" fn gain_writing(field: usize) -> i32 {
    GAINS.with_borrow(|gains| gains.writing[field])
}

#[no_mangle]
pub extern "C" fn gain_success(mic: i32, music: i32, mic_muted: i32, music_muted: i32) {
    GAINS.with_borrow_mut(|gains| gains.success([mic, music, mic_muted, music_muted]));
}

#[no_mangle]
pub extern "C" fn gain_failure(retain: bool) {
    GAINS.with_borrow_mut(|gains| gains.failure(retain));
}

#[no_mangle]
pub extern "C" fn gain_online() -> bool {
    GAINS.with_borrow(|gains| gains.online)
}

#[no_mangle]
pub extern "C" fn gain_pending() -> bool {
    GAINS.with_borrow(ControlQueue::has_pending)
}

struct Slider {
    min: i32,
    max: i32,
    step: f64,
}

impl Slider {
    fn new(min: i32, max: i32, preference: i32, depth: bool) -> Self {
        let step = match preference {
            0 => 1.0,
            5 | 10 | 20 if depth => f64::from((max - min) * preference) / 100.0,
            5 | 10 | 20 => f64::from(preference),
            _ => 1.0,
        };
        Self { min, max, step }
    }

    fn stops(&self) -> impl Iterator<Item = i32> + '_ {
        let range = f64::from(self.max - self.min);
        (0..)
            .map(|index| f64::from(index) * self.step)
            .take_while(move |&offset| offset < range)
            .map(|offset| (f64::from(self.min) + offset).round() as i32)
            .chain(std::iter::once(self.max))
    }

    fn nearest(&self, value: f64) -> i32 {
        self.stops().fold(self.min, |best, stop| {
            if (value - f64::from(stop)).abs() <= (value - f64::from(best)).abs() {
                stop
            } else {
                best
            }
        })
    }

    fn key(&self, value: f64, key: i32) -> i32 {
        let up = || {
            self.stops()
                .find(|&stop| f64::from(stop) > value)
                .unwrap_or(self.max)
        };
        let down = || {
            self.stops()
                .take_while(|&stop| f64::from(stop) < value)
                .last()
                .unwrap_or(self.min)
        };
        let page = f64::from(self.max - self.min) / 10.0;
        match key {
            1 => up(),
            2 => down(),
            3 => self.min,
            4 => self.max,
            5 => up().max(self.nearest(value + page)),
            6 => down().min(self.nearest(value - page)),
            _ => self.nearest(value),
        }
    }
}

#[no_mangle]
pub extern "C" fn slider_value(
    min: i32,
    max: i32,
    preference: i32,
    depth: bool,
    value: f64,
    key: i32,
) -> i32 {
    Slider::new(min, max, preference, depth).key(value, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimistic_edits_survive_polls_and_a_failed_write_without_overwriting_newer_edits() {
        let mut gains = ControlQueue::new();
        assert_eq!(gains.next(true, true), Request::Read);
        gains.pending[1] = 140;
        assert_eq!(gains.next(true, true), Request::None);
        gains.success([100, 130, 0, 0]);
        assert_eq!(gains.shown(1), 140);
        assert_eq!(gains.next(true, false), Request::Write);
        gains.pending[1] = 180;
        gains.pending[2] = 1;
        gains.failure(true);
        assert_eq!(gains.next(false, false), Request::Read);
        gains.success([100, 130, 0, 0]);
        assert_eq!(gains.next(true, false), Request::Write);
        assert_eq!(gains.writing, [ABSENT, 180, 1, ABSENT]);
        gains.success([100, 180, 1, 0]);
        assert_eq!(gains.shown(1), 180);
        assert_eq!(gains.next(true, false), Request::None);
    }

    #[test]
    fn wrist_disconnect_discards_unconfirmed_commands() {
        let mut gains = ControlQueue::new();
        gains.success([100, 130, 0, 0]);
        gains.pending[1] = 140;
        assert_eq!(gains.next(true, false), Request::Write);
        gains.pending[3] = 1;
        gains.failure(false);
        assert!(!gains.has_pending());
        assert_eq!(gains.shown(1), 130);
        assert_eq!(gains.shown(3), 0);
    }

    #[test]
    fn slider_supports_percentage_depth_page_keys_endpoints_and_off_grid_values() {
        let gain = Slider::new(0, 200, 10, false);
        assert_eq!(gain.key(155.0, 0), 160);
        assert_eq!(gain.key(157.0, 2), 150);
        assert_eq!(gain.key(157.0, 5), 180);
        assert_eq!(gain.key(200.0, 1), 200);
        let depth = Slider::new(1, 24, 20, true);
        assert_eq!(depth.stops().collect::<Vec<_>>(), [1, 6, 10, 15, 19, 24]);
        assert_eq!(depth.key(12.0, 2), 10);
        assert_eq!(depth.key(12.0, 4), 24);
        let smooth = Slider::new(0, 200, 0, false);
        assert_eq!(smooth.key(157.0, 1), 158);
    }
}
// Duck queue fields are enabled, depth and conditional migration.
struct DuckControls {
    queue: ControlQueue,
    status: i32,
    migrated: bool,
}

thread_local! {
    static DUCK: RefCell<DuckControls> = const { RefCell::new(DuckControls {
        queue: ControlQueue::new(),
        status: 0,
        migrated: false,
    }) };
}

#[no_mangle]
pub extern "C" fn duck_change(enabled: i32, depth: i32) {
    DUCK.with_borrow_mut(|duck| {
        if enabled != ABSENT {
            duck.queue.pending[0] = enabled;
        }
        if depth != ABSENT {
            duck.queue.pending[1] = depth;
        }
        duck.queue.pending[2] = ABSENT;
    });
}

#[no_mangle]
pub extern "C" fn duck_migrate(depth: i32, configured: bool) {
    DUCK.with_borrow_mut(|duck| {
        if !configured && !duck.migrated {
            duck.migrated = true;
            if (1..=24).contains(&depth) && !duck.queue.has_pending() {
                duck.queue.pending[1] = depth;
                duck.queue.pending[2] = 1;
            }
        }
    });
}

#[no_mangle]
pub extern "C" fn duck_next(can_write: bool, poll: bool) -> i32 {
    DUCK.with_borrow_mut(|duck| duck.queue.next(can_write, poll) as i32)
}

#[no_mangle]
pub extern "C" fn duck_writing(field: usize) -> i32 {
    DUCK.with_borrow(|duck| duck.queue.writing[field])
}

#[no_mangle]
pub extern "C" fn duck_success(enabled: i32, depth: i32, status: i32) {
    DUCK.with_borrow_mut(|duck| {
        duck.queue.success([enabled, depth, ABSENT, ABSENT]);
        duck.status = status;
    });
}

#[no_mangle]
pub extern "C" fn duck_failure() {
    DUCK.with_borrow_mut(|duck| {
        // Keep ordinary edits across a stale token or transient outage. Migration
        // is conditional on the host still having no config, so it cannot clobber it.
        if duck.queue.writing[2] != ABSENT {
            duck.queue.writing = [ABSENT; FIELDS];
            duck.migrated = false;
        }
        duck.queue.failure(true);
        duck.status = 0;
    });
}

#[no_mangle]
pub extern "C" fn duck_enabled() -> bool {
    DUCK.with_borrow(|duck| duck.queue.shown(0) == 1)
}

#[no_mangle]
pub extern "C" fn duck_depth() -> i32 {
    DUCK.with_borrow(|duck| duck.queue.shown(1))
}

#[no_mangle]
pub extern "C" fn duck_online() -> bool {
    DUCK.with_borrow(|duck| duck.queue.online)
}

#[no_mangle]
pub extern "C" fn duck_poll_ms() -> i32 {
    DUCK.with_borrow(|duck| {
        if matches!(duck.status, 6 | 7) {
            500
        } else {
            2500
        }
    })
}

#[derive(Default)]
struct PreferenceSync {
    dirty: u32,
    sending: u32,
    changed_while_sending: u32,
    migrated: bool,
}

impl PreferenceSync {
    fn change(&mut self, keys: u32) {
        self.dirty |= keys;
        self.changed_while_sending |= keys & self.sending;
    }

    fn begin_write(&mut self) -> u32 {
        self.sending = self.dirty;
        self.changed_while_sending = 0;
        self.sending
    }

    fn acknowledge(&mut self) -> u32 {
        let confirmed = self.sending & !self.changed_while_sending;
        self.dirty &= !confirmed;
        self.sending = 0;
        confirmed
    }

    fn migration_keys(&self, legacy: u32, saved: u32) -> u32 {
        if self.migrated {
            return 0;
        }
        legacy & !saved & !self.dirty
    }

    fn needs_read(&self, has_token: bool) -> bool {
        !has_token || !self.migrated || self.dirty == 0
    }
}

thread_local! {
    static PREFS: RefCell<PreferenceSync> = RefCell::new(PreferenceSync::default());
}

#[no_mangle]
pub extern "C" fn preferences_change(keys: u32) {
    PREFS.with_borrow_mut(|prefs| prefs.change(keys));
}

#[no_mangle]
pub extern "C" fn preferences_begin_write() -> u32 {
    PREFS.with_borrow_mut(PreferenceSync::begin_write)
}

#[no_mangle]
pub extern "C" fn preferences_acknowledge() -> u32 {
    PREFS.with_borrow_mut(PreferenceSync::acknowledge)
}

#[no_mangle]
pub extern "C" fn preferences_migration_keys(legacy: u32, saved: u32) -> u32 {
    PREFS.with_borrow(|prefs| prefs.migration_keys(legacy, saved))
}

#[no_mangle]
pub extern "C" fn preferences_finish_migration() {
    PREFS.with_borrow_mut(|prefs| prefs.migrated = true);
}

#[no_mangle]
pub extern "C" fn preferences_read(has_token: bool) -> bool {
    PREFS.with_borrow(|prefs| prefs.needs_read(has_token))
}

#[cfg(test)]
mod preference_tests {
    use super::*;

    #[test]
    fn migrate_only_missing_keys_and_confirm_only_edits_in_the_sent_batch() {
        let mut prefs = PreferenceSync::default();
        let theme = 1;
        let snap = 2;
        let close_to_tray = 4;
        let image = 8;
        prefs.change(snap);
        assert!(prefs.needs_read(true));
        assert_eq!(prefs.migration_keys(theme | snap | image, theme), image);
        prefs.migrated = true;
        assert_eq!(prefs.migration_keys(theme | image, theme), 0);
        assert!(!prefs.needs_read(true));
        assert!(prefs.needs_read(false));
        assert_eq!(prefs.begin_write(), snap);
        prefs.change(snap | close_to_tray);
        assert_eq!(prefs.acknowledge(), 0);
        assert_eq!(prefs.begin_write(), snap | close_to_tray);
        assert_eq!(prefs.acknowledge(), snap | close_to_tray);
        assert!(prefs.needs_read(true));
    }
}

#[derive(Default)]
struct ActionGate {
    version: u32,
    reading: u32,
    writing: bool,
}

impl ActionGate {
    fn advance(&mut self) -> u32 {
        self.version = self.version.wrapping_add(1).max(1);
        self.version
    }

    fn begin_read(&mut self) -> u32 {
        if self.reading != 0 || self.writing {
            return 0;
        }
        self.reading = self.advance();
        self.reading
    }

    fn begin_write(&mut self, cancel_read: bool) -> bool {
        if self.writing {
            return false;
        }
        self.advance();
        if cancel_read {
            self.reading = 0;
        }
        self.writing = true;
        true
    }

    fn finish_read(&mut self, version: u32) {
        if self.reading == version {
            self.reading = 0;
        }
    }

    fn cancel_read(&mut self) {
        self.advance();
        self.reading = 0;
    }

    fn is_current(&self, version: u32) -> bool {
        self.version == version && self.reading == version
    }
}

thread_local! {
    // Spotify status, microphone status and the setup/diagnostics scan.
    static ACTIONS: RefCell<[ActionGate; 3]> = RefCell::new(std::array::from_fn(|_| ActionGate::default()));
}

#[no_mangle]
pub extern "C" fn action_read(channel: usize) -> u32 {
    ACTIONS.with_borrow_mut(|gates| gates[channel].begin_read())
}

#[no_mangle]
pub extern "C" fn action_current(channel: usize, version: u32) -> bool {
    ACTIONS.with_borrow(|gates| gates[channel].is_current(version))
}

#[no_mangle]
pub extern "C" fn action_reading(channel: usize) -> bool {
    ACTIONS.with_borrow(|gates| gates[channel].reading != 0)
}

#[no_mangle]
pub extern "C" fn action_finish_read(channel: usize, version: u32) {
    ACTIONS.with_borrow_mut(|gates| gates[channel].finish_read(version));
}

#[no_mangle]
pub extern "C" fn action_cancel_read(channel: usize) {
    ACTIONS.with_borrow_mut(|gates| gates[channel].cancel_read());
}

#[no_mangle]
pub extern "C" fn action_write(channel: usize) -> bool {
    ACTIONS.with_borrow_mut(|gates| gates[channel].begin_write(channel == 1))
}

#[no_mangle]
pub extern "C" fn action_writing(channel: usize) -> bool {
    ACTIONS.with_borrow(|gates| gates[channel].writing)
}

#[no_mangle]
pub extern "C" fn action_finish_write(channel: usize) {
    ACTIONS.with_borrow_mut(|gates| gates[channel].writing = false);
}

#[cfg(test)]
mod action_tests {
    use super::*;

    #[test]
    fn stale_poll_cannot_override_write_or_clear_the_next_poll() {
        let mut gate = ActionGate::default();
        let first = gate.begin_read();
        assert_ne!(first, 0);
        assert_eq!(gate.begin_read(), 0);
        assert!(gate.begin_write(true));
        assert!(!gate.begin_write(true));
        assert_eq!(gate.begin_read(), 0);
        gate.writing = false;
        let next = gate.begin_read();
        gate.finish_read(first);
        assert!(gate.is_current(next));
        assert!(!gate.is_current(first));
        gate.cancel_read();
        assert!(!gate.is_current(next));
    }
}
mod appearance;
