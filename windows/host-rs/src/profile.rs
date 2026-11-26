use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use serde_core::de::{Deserialize, Deserializer, Error as _, MapAccess, Visitor};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_GEN_FAILURE, ERROR_INVALID_PARAMETER, ERROR_LOCK_VIOLATION,
    ERROR_PATH_NOT_FOUND,
};
use windows_sys::Win32::Storage::FileSystem::{
    LockFileEx, UnlockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
};
use windows_sys::Win32::System::IO::OVERLAPPED;

const LOCKFILE: &str = ".settings-lock";
const BACKUP: &str = "settings-backup.json";
const BAD_JSON: u32 = 0x10000;
const BAD_SCHEMA: u32 = 0x10001;
const CALLBACK_ERROR: u32 = 0x10002;
const MIRROR_FAILED: u32 = 1;
const RESTORE_FAILED: u32 = 2;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct ProfileLock(File);

impl ProfileLock {
    fn acquire(folder: &Path) -> io::Result<Self> {
        fs::create_dir_all(folder)?;
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(folder.join(LOCKFILE))?;
        // The existing msvcrt writers lock the first byte at offset zero.
        for attempt in 0..=10 {
            let mut span: OVERLAPPED = unsafe { std::mem::zeroed() };
            let status = unsafe {
                LockFileEx(
                    file.as_raw_handle(),
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    1,
                    0,
                    &mut span,
                )
            };
            if status != 0 {
                return Ok(Self(file));
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_LOCK_VIOLATION as i32) || attempt == 10 {
                return Err(error);
            }
            thread::sleep(Duration::from_secs(1));
        }
        unreachable!("the final locking attempt returns");
    }
}

impl Drop for ProfileLock {
    fn drop(&mut self) {
        let mut span: OVERLAPPED = unsafe { std::mem::zeroed() };
        // Unlock the same one-byte region acquired by this handle.
        unsafe { UnlockFileEx(self.0.as_raw_handle(), 0, 1, 0, &mut span) };
    }
}

struct TemporaryProfile(Option<PathBuf>);

impl Drop for TemporaryProfile {
    fn drop(&mut self) {
        if let Some(path) = self.0.as_ref() {
            let _ = fs::remove_file(path);
        }
    }
}

fn temporary_file(folder: &Path) -> io::Result<(File, TemporaryProfile)> {
    for _ in 0..32 {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = folder.join(format!(".settings-{}-{sequence}.tmp", std::process::id()));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => return Ok((file, TemporaryProfile(Some(path)))),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        "Could not allocate profile staging file",
    ))
}

fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let folder = path
        .parent()
        .ok_or_else(|| io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as i32))?;
    let (mut file, mut temporary) = temporary_file(folder)?;
    let result = file.write_all(data).and_then(|()| file.sync_all());
    drop(file);
    result?;
    fs::rename(temporary.0.as_ref().unwrap(), path)?;
    temporary.0 = None;
    Ok(())
}

fn error_number(error: io::Error) -> u32 {
    error.raw_os_error().unwrap_or(ERROR_GEN_FAILURE as i32) as u32
}

struct UniqueObject<'a>(BTreeMap<String, &'a RawValue>);

impl<'de> Deserialize<'de> for UniqueObject<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Unique;
        impl<'de> Visitor<'de> for Unique {
            type Value = UniqueObject<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an object with unique settings keys")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = BTreeMap::new();
                while let Some((name, value)) = map.next_entry::<String, &'de RawValue>()? {
                    if values.insert(name, value).is_some() {
                        return Err(A::Error::custom("Duplicate settings field"));
                    }
                }
                Ok(UniqueObject(values))
            }
        }
        deserializer.deserialize_map(Unique)
    }
}

fn strict_value(raw: &RawValue, depth: usize) -> Result<Value, u32> {
    if depth > 128 {
        return Err(BAD_JSON);
    }
    let text = raw.get();
    match text.as_bytes().first() {
        Some(b'{') => serde_json::from_str::<UniqueObject<'_>>(text)
            .map_err(|_| BAD_JSON)?
            .0
            .into_iter()
            .map(|(name, value)| Ok((name, strict_value(value, depth + 1)?)))
            .collect::<Result<Map<_, _>, _>>()
            .map(Value::Object),
        Some(b'[') => serde_json::from_str::<Vec<&RawValue>>(text)
            .map_err(|_| BAD_JSON)?
            .into_iter()
            .map(|value| strict_value(value, depth + 1))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Some(b'-' | b'0'..=b'9') if text.contains(['.', 'e', 'E']) => {
            if !text.parse::<f64>().is_ok_and(f64::is_finite) {
                return Err(BAD_JSON);
            }
            serde_json::from_str(text).map_err(|_| BAD_JSON)
        }
        _ => serde_json::from_str(text).map_err(|_| BAD_JSON),
    }
}

fn decode(data: &[u8]) -> Result<Map<String, Value>, u32> {
    let data = data.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(data);
    let raw: &RawValue = serde_json::from_slice(data).map_err(|_| BAD_JSON)?;
    match strict_value(raw, 0)? {
        Value::Object(value) => Ok(value),
        _ => Err(BAD_JSON),
    }
}

fn read(path: &Path) -> Result<Map<String, Value>, u32> {
    decode(&fs::read(path).map_err(error_number)?)
}

fn write(path: &Path, data: &Map<String, Value>) -> Result<(), u32> {
    atomic_write(path, &serde_json::to_vec(data).map_err(|_| BAD_JSON)?).map_err(error_number)
}

fn backup_sections(path: &Path) -> Result<Map<String, Value>, u32> {
    let mut backup = match read(path) {
        Ok(backup) => backup,
        Err(BAD_JSON | ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => return Ok(Map::new()),
        Err(status) => return Err(status),
    };
    let legacy_one = backup
        .get("version")
        .is_some_and(|value| value.as_f64() == Some(1.0) || value.as_bool() == Some(true));
    if legacy_one {
        if let Some(Value::Object(settings)) = backup.remove("settings") {
            return Ok(settings);
        }
    }
    Ok(Map::new())
}

fn mirror(path: &Path, key: &str, data: &Map<String, Value>, warnings: &mut u32) {
    let result = || {
        let mut sections = backup_sections(path)?;
        let value = Value::Object(data.clone());
        if sections.get(key) != Some(&value) {
            sections.insert(key.to_owned(), value);
            write(
                path,
                &Map::from_iter([
                    ("version".into(), Value::from(1)),
                    ("settings".into(), Value::Object(sections)),
                ]),
            )?;
        }
        Ok::<_, u32>(())
    };
    if result().is_err() {
        *warnings |= MIRROR_FAILED;
    }
}

type Validate = extern "C" fn(*const u8, usize) -> u32;

fn validate(data: &Map<String, Value>, callback: Validate) -> Result<(), u32> {
    let encoded = serde_json::to_vec(data).map_err(|_| BAD_JSON)?;
    match callback(encoded.as_ptr(), encoded.len()) {
        0 => Ok(()),
        1 => Err(BAD_SCHEMA),
        _ => Err(CALLBACK_ERROR),
    }
}

fn recoverable(status: u32) -> bool {
    matches!(
        status,
        BAD_JSON | BAD_SCHEMA | ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND
    )
}

fn load(
    path: &Path,
    backup: &Path,
    key: &str,
    check: Validate,
    warnings: &mut u32,
) -> Result<Map<String, Value>, u32> {
    let primary = read(path).and_then(|data| {
        validate(&data, check)?;
        Ok(data)
    });
    match primary {
        Ok(data) => {
            mirror(backup, key, &data, warnings);
            Ok(data)
        }
        Err(status) if recoverable(status) => {
            let Some(section) = backup_sections(backup)?.remove(key) else {
                return Err(status);
            };
            let Value::Object(data) = section else {
                return Err(BAD_JSON);
            };
            validate(&data, check)?;
            if write(path, &data).is_err() {
                *warnings |= RESTORE_FAILED;
            }
            Ok(data)
        }
        Err(status) => Err(status),
    }
}

fn transaction(
    path: &Path,
    operation: u32,
    data: &[u8],
    check: Option<Validate>,
    warnings: &mut u32,
) -> Result<(), u32> {
    let folder = path.parent().ok_or(ERROR_INVALID_PARAMETER)?;
    let key = path
        .file_name()
        .and_then(|key| key.to_str())
        .ok_or(ERROR_INVALID_PARAMETER)?;
    let backup = folder.join(BACKUP);
    let _lock = ProfileLock::acquire(folder).map_err(error_number)?;
    match operation {
        0 => {
            let data = decode(data)?;
            write(path, &data)?;
            mirror(&backup, key, &data, warnings);
        }
        1 => {
            load(
                path,
                &backup,
                key,
                check.ok_or(ERROR_INVALID_PARAMETER)?,
                warnings,
            )?;
        }
        2 => {
            let check = check.ok_or(ERROR_INVALID_PARAMETER)?;
            let changes = decode(data)?;
            let previous = match load(path, &backup, key, check, warnings) {
                Ok(data) => data,
                Err(status) if recoverable(status) => Map::new(),
                Err(status) => return Err(status),
            };
            let mut merged = previous.clone();
            merged.extend(changes);
            validate(&merged, check)?;
            if merged != previous {
                write(path, &merged)?;
                mirror(&backup, key, &merged, warnings);
            }
        }
        _ => return Err(ERROR_INVALID_PARAMETER),
    }
    Ok(())
}

/// The path span, supplied data span, and writable warnings pointer must live through the call.
/// The optional callback runs synchronously and borrows its input only until it returns.
#[no_mangle]
pub unsafe extern "C" fn lummix_profile_transaction(
    path: *const u16,
    path_len: usize,
    operation: u32,
    data: *const u8,
    data_len: usize,
    check: Option<Validate>,
    warnings: *mut u32,
) -> u32 {
    if path.is_null() || path_len == 0 || warnings.is_null() || (data.is_null() && data_len != 0) {
        return ERROR_INVALID_PARAMETER;
    }
    let path = PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(
        path, path_len,
    )));
    let data = if data_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(data, data_len)
    };
    *warnings = 0;
    match transaction(&path, operation, data, check, &mut *warnings) {
        Ok(()) => 0,
        Err(status) => status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("LummixHostProfile-{}-{label}", std::process::id(),));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn settings(&self) -> PathBuf {
            self.0.join("output-gains.json")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    extern "C" fn validate_gain(data: *const u8, len: usize) -> u32 {
        let input = unsafe { std::slice::from_raw_parts(data, len) };
        let parsed = serde_json::from_slice::<Value>(input).unwrap_or(Value::Null);
        match parsed["revision"].as_u64() {
            Some(_) => 0,
            None => 1,
        }
    }

    fn call(
        path: &Path,
        operation: u32,
        data: &[u8],
        check: Option<Validate>,
        warnings: &mut u32,
    ) -> u32 {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        unsafe {
            lummix_profile_transaction(
                wide.as_ptr(),
                wide.len(),
                operation,
                data.as_ptr(),
                data.len(),
                check,
                warnings,
            )
        }
    }

    #[test]
    fn rejects_bad_fields_and_keeps_legacy_bom_unicode_and_large_integers() {
        for invalid in [
            br#"{"nested":[{"duplicate":1,"duplicate":2}]}"#.as_slice(),
            br#"{"data": [1e999]}"#.as_slice(),
            br#"{"data": NaN}"#.as_slice(),
            br#"null"#.as_slice(),
        ] {
            assert_eq!(decode(invalid), Err(BAD_JSON));
        }
        let input = "\u{feff}{\"wide\":184467440737095516160,\"text\":\"Mix \u{1f3a7}\"}";
        let decoded = decode(input.as_bytes()).unwrap();
        assert_eq!(decoded["wide"].to_string(), "184467440737095516160");
        assert_eq!(decoded["text"], "Mix \u{1f3a7}");
    }

    #[test]
    fn native_abi_imports_a_legacy_file_and_merges_updates_under_the_lock() {
        let fixture = Fixture::new("updating");
        let profile = fixture.settings();
        fs::write(&profile, br#"{"revision":7,"gains":{"micGain":104}}"#).unwrap();
        let mut warnings = u32::MAX;
        assert_eq!(
            call(&profile, 1, &[], Some(validate_gain), &mut warnings),
            0
        );
        assert_eq!(warnings, 0);
        let backup = fixture.0.join(BACKUP);
        assert_eq!(
            backup_sections(&backup).unwrap()["output-gains.json"]["revision"],
            7
        );
        fs::write(&profile, br#"{"revision":-1}"#).unwrap();
        assert_eq!(
            call(
                &profile,
                2,
                br#"{"revision":8}"#,
                Some(validate_gain),
                &mut warnings
            ),
            0
        );
        let saved = read(&profile).unwrap();
        assert_eq!(saved["revision"], 8);
        assert_eq!(saved["gains"]["micGain"], 104);
        assert_eq!(
            backup_sections(&backup).unwrap()["output-gains.json"],
            Value::Object(saved)
        );
    }

    #[test]
    fn native_abi_returns_valid_backup_even_when_legacy_primary_is_locked() {
        let fixture = Fixture::new("locked");
        let profile = fixture.settings();
        let mut warnings = 0;
        assert_eq!(
            call(&profile, 0, br#"{"revision":5}"#, None, &mut warnings),
            0
        );
        fs::write(&profile, b"null").unwrap();
        let locked = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&profile)
            .unwrap();
        assert_eq!(
            call(&profile, 1, &[], Some(validate_gain), &mut warnings),
            0
        );
        assert_eq!(warnings, RESTORE_FAILED);
        assert_eq!(fs::read(&profile).unwrap(), b"null");
        drop(locked);
        assert_eq!(
            call(&profile, 1, &[], Some(validate_gain), &mut warnings),
            0
        );
        assert_eq!(warnings, 0);
        assert_eq!(read(&profile).unwrap()["revision"], 5);
        assert_eq!(fixture.0.read_dir().unwrap().count(), 3);
    }
}
