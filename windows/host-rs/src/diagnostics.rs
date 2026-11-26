use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::Path;

const LIMIT: u64 = 64 * 1024;
const LOG: &str = "lummix.log";
const PREVIOUS: &str = "lummix.previous.log";

#[repr(C)]
#[derive(Default)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[repr(C)]
struct OsVersion {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    service_pack: [u16; 128],
}

#[link(name = "kernel32")]
extern "system" {
    fn GetSystemTime(time: *mut SystemTime);
}

#[link(name = "ntdll")]
extern "system" {
    fn RtlGetVersion(version: *mut OsVersion) -> i32;
}

fn log_line(version: &str, event: &str) -> io::Result<String> {
    let mut time = SystemTime::default();
    let mut windows = OsVersion {
        size: std::mem::size_of::<OsVersion>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service_pack: [0; 128],
    };
    // Both ABI buffers are writable and sized for the structures Win32 expects.
    unsafe { GetSystemTime(&mut time) };
    if unsafe { RtlGetVersion(&mut windows) } != 0 {
        return Err(io::Error::other("Could not read Windows build"));
    }
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z Lummix={version} WinBuild={} {event}\n",
        time.year, time.month, time.day, time.hour, time.minute, time.second, windows.build
    ))
}

fn valid_version(version: &str) -> bool {
    let fields: Vec<_> = version.split('.').collect();
    fields.len() == 3
        && fields.iter().all(|field| {
            (1..=4).contains(&field.len())
                && field.bytes().all(|b| b.is_ascii_digit())
                && (field.len() == 1 || !field.starts_with('0'))
        })
}

fn valid_event(event: &str) -> bool {
    matches!(
        event,
        "opened"
            | "closed"
            | "startup_failed"
            | "studio_failed"
            | "wrist_failed"
            | "spotify_failed"
            | "window_failed"
            | "spotify_setup_failed"
            | "update_available"
            | "update_verified"
            | "update_network"
            | "update_invalid"
            | "update_storage"
            | "update_busy"
            | "update_launched"
            | "update_blocked"
            | "audio_status_unavailable"
    )
}

fn valid_snapshot(mix: &str, cable: &str, spotify: &str) -> bool {
    matches!(mix, "yes" | "no" | "unknown")
        && matches!(cable, "yes" | "no" | "unknown")
        && matches!(
            spotify,
            "off"
                | "connecting"
                | "unsupported"
                | "waiting_spotify"
                | "direct_to_cable"
                | "unavailable"
                | "receiving"
                | "waiting_audio"
                | "ambiguous"
                | "unknown"
        )
}

fn append(root: &Path, line: &str) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let path = root.join(LOG);
    match path.metadata() {
        Ok(info) if info.len() + line.len() as u64 > LIMIT => {
            fs::rename(&path, root.join(PREVIOUS))?
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

fn clear(root: &Path) -> io::Result<()> {
    for file in [LOG, PREVIOUS] {
        match fs::remove_file(root.join(file)) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub fn run(args: &[OsString]) -> io::Result<()> {
    let profile = env::var_os("LOCALAPPDATA")
        .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "Missing LOCALAPPDATA"))?;
    let root = Path::new(&profile).join("LumMix").join("Diagnostics");
    let args: Option<Vec<_>> = args.iter().map(|value| value.to_str()).collect();
    let invalid = || io::Error::new(ErrorKind::InvalidInput, "Invalid diagnostic command");
    match args.ok_or_else(invalid)?.as_slice() {
        ["clear"] => clear(&root),
        ["record", version, event] if valid_version(version) && valid_event(event) => {
            append(&root, &log_line(version, event)?)
        }
        ["snapshot", version, mix, cable, spotify]
            if valid_version(version) && valid_snapshot(mix, cable, spotify) =>
        {
            let event = format!("audio_snapshot mix={mix} cable={cable} spotify={spotify}");
            append(&root, &log_line(version, &event)?)
        }
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_fields_cannot_contain_private_values_or_newlines() {
        assert!(valid_version("1.0.9"));
        assert!(!valid_version("1.0.9\n"));
        assert!(!valid_version("01.0.9"));
        assert!(valid_event("opened"));
        assert!(!valid_event("C:/Private Song.mp3"));
        assert!(valid_snapshot("yes", "no", "receiving"));
        assert!(!valid_snapshot("yes", "no", "C:/Private Song.mp3"));
        let line = log_line("1.0.9", "opened").unwrap();
        assert!(line.contains("Z Lummix=1.0.9 WinBuild="));
        assert!(line.ends_with(" opened\n"));
    }

    #[test]
    fn rotation_replaces_old_archive_and_clear_only_removes_logs() {
        let root = env::temp_dir().join(format!("LummixHostTest-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let full = "a".repeat(LIMIT as usize);
        fs::write(root.join(LOG), &full).unwrap();
        fs::write(root.join(PREVIOUS), "older").unwrap();
        fs::write(root.join("other"), "leave").unwrap();
        append(&root, "opened\n").unwrap();
        assert_eq!(fs::read_to_string(root.join(PREVIOUS)).unwrap(), full);
        assert_eq!(fs::read_to_string(root.join(LOG)).unwrap(), "opened\n");
        clear(&root).unwrap();
        clear(&root).unwrap();
        assert!(!root.join(LOG).exists());
        assert!(!root.join(PREVIOUS).exists());
        assert_eq!(fs::read_to_string(root.join("other")).unwrap(), "leave");
        fs::remove_file(root.join("other")).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
