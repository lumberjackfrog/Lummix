use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use windows::{
    core::{Error, Result, HRESULT},
    Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG},
};

const LIFETIME: Duration = Duration::from_secs(600);
const TOKEN_LIMIT: usize = 64;

struct Capability {
    key: String,
    origin: String,
    expires: Instant,
}

#[derive(Default)]
struct Capabilities {
    entries: VecDeque<Capability>,
}

impl Capabilities {
    fn issue(&mut self, key: String, origin: &str, now: Instant) -> Vec<String> {
        let mut evicted = Vec::new();
        while self
            .entries
            .front()
            .is_some_and(|entry| entry.expires <= now)
            || self.entries.len() >= TOKEN_LIMIT
        {
            evicted.push(self.entries.pop_front().unwrap().key);
        }
        self.entries.push_back(Capability {
            key,
            origin: origin.to_owned(),
            expires: now + LIFETIME,
        });
        evicted
    }

    fn valid(&self, key: &str, origin: &str, now: Instant) -> bool {
        key.len() == 43
            && self
                .entries
                .iter()
                .any(|entry| key == entry.key && origin == entry.origin && now < entry.expires)
    }
}

#[derive(Default)]
struct Stores {
    next: u64,
    stores: HashMap<u64, Capabilities>,
}

pub(super) fn apply(operation: u32, store: u64, request: &Value) -> Result<Value> {
    static STORES: OnceLock<Mutex<Stores>> = OnceLock::new();
    let mut registry = STORES.get_or_init(Mutex::default).lock().unwrap();
    if operation == 0 {
        registry.next += 1;
        let store = registry.next;
        registry.stores.insert(store, Capabilities::default());
        return Ok(json!(store));
    }
    if operation == 4 {
        registry.stores.remove(&store);
        return Ok(Value::Null);
    }
    let entries = registry
        .stores
        .get_mut(&store)
        .ok_or_else(|| Error::from_hresult(HRESULT(0x80070006u32 as i32)))?;
    let origin = request["origin"].as_str().unwrap_or_default();
    let token = request["token"].as_str().unwrap_or_default();
    let now = Instant::now();
    match operation {
        1 => {
            let mut bytes = [0u8; 32];
            unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG).ok()? };
            let key = URL_SAFE_NO_PAD.encode(bytes);
            let evicted = entries.issue(key.clone(), origin, now);
            Ok(json!({"token": key, "evicted": evicted}))
        }
        2 => Ok(json!(entries.valid(token, origin, now))),
        3 => {
            entries.entries.retain(|entry| entry.key != token);
            Ok(Value::Null)
        }
        _ => Err(Error::from_hresult(HRESULT(0x80070057u32 as i32))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_scopes_expires_and_revokes_capabilities() {
        let mut entries = Capabilities::default();
        let now = Instant::now();
        for index in 0..=TOKEN_LIMIT {
            let key = format!("{index:043}");
            let evicted = entries.issue(key, "studio", now);
            assert_eq!(evicted.len(), usize::from(index == TOKEN_LIMIT));
        }
        assert!(!entries.valid(&format!("{:043}", 0), "studio", now));
        let key = format!("{:043}", TOKEN_LIMIT);
        assert!(entries.valid(&key, "studio", now));
        assert!(!entries.valid(&key, "wrist", now));
        assert!(!entries.valid(&key, "studio", now + LIFETIME));
    }
}
