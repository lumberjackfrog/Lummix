use crate::vr_catalog as catalog;
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    env,
    fs::{self, File},
    io::{self, Read},
    mem::size_of,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    ptr::{null, null_mut},
    slice,
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_Registry_PropertyW, CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW,
        CM_Locate_DevNodeW, CM_DRP_CLASS, CM_DRP_DEVICEDESC, CM_DRP_FRIENDLYNAME,
        CM_GETIDLIST_FILTER_PRESENT, CR_BUFFER_SMALL, CR_SUCCESS,
    },
    Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE, SYSTEMTIME},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
        SystemInformation::GetSystemTime,
        Threading::CREATE_NO_WINDOW,
    },
};
const RUNTIMES: &[(&str, &str)] = &[
    ("vrserver", "SteamVR"),
    ("ovrserver_x64", "Meta Quest Link runtime"),
    ("virtualdesktop.streamer", "Virtual Desktop Streamer"),
    ("picoconnect", "PICO Connect"),
    ("streamingassistant", "PICO Streaming Assistant"),
    ("piserver", "Pimax runtime"),
    ("vrcompositor", "SteamVR compositor"),
];
const MAX_STEAM_PROFILE: u64 = 4_000_000;
const CACHE_TIME: Duration = Duration::from_secs(10);

pub fn steam_root() -> PathBuf {
    env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:/Program Files (x86)"))
        .join("Steam")
}

fn wide_string(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

struct Snapshot(HANDLE);

impl Drop for Snapshot {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub fn processes() -> io::Result<BTreeSet<String>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = Snapshot(snapshot);
    let mut row: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    row.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    if unsafe { Process32FirstW(snapshot.0, &mut row) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut names = BTreeSet::new();
    loop {
        let exe = wide_string(&row.szExeFile).to_lowercase();
        names.insert(exe.strip_suffix(".exe").unwrap_or(&exe).to_owned());
        if unsafe { Process32NextW(snapshot.0, &mut row) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                return Err(error);
            }
            break;
        }
    }
    Ok(names)
}

fn devices_present() -> io::Result<Vec<u16>> {
    // The present list can grow between sizing and retrieval during hotplug.
    for _ in 0..3 {
        let mut length = 0;
        let result = unsafe {
            CM_Get_Device_ID_List_SizeW(&mut length, null(), CM_GETIDLIST_FILTER_PRESENT)
        };
        if result != CR_SUCCESS || length > 1_000_000 {
            break;
        }
        if length == 0 {
            return Ok(Vec::new());
        }
        let mut devices = vec![0; length as usize];
        let result = unsafe {
            CM_Get_Device_ID_ListW(
                null(),
                devices.as_mut_ptr(),
                length,
                CM_GETIDLIST_FILTER_PRESENT,
            )
        };
        match result {
            CR_SUCCESS => return Ok(devices),
            CR_BUFFER_SMALL => continue,
            _ => break,
        }
    }
    Err(io::Error::other("Windows device inventory unavailable"))
}

fn property(device: u32, field: u32) -> Option<String> {
    let mut data = [0u16; 1024];
    let mut size = std::mem::size_of_val(&data) as u32;
    let status = unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            device,
            field,
            null_mut(),
            data.as_mut_ptr().cast(),
            &mut size,
            0,
        )
    };
    (status == CR_SUCCESS).then(|| wide_string(&data[..(size as usize / 2).min(data.len())]))
}

fn physical() -> io::Result<BTreeSet<String>> {
    let devices = devices_present()?;
    let mut names = BTreeSet::new();
    let mut index = 0;
    while index < devices.len() {
        let id = &devices[index..];
        let Some(end) = id.iter().position(|&unit| unit == 0) else {
            break;
        };
        if end == 0 {
            break;
        }
        index += end + 1;
        let internal_id = wide_string(id);
        let bus = internal_id
            .split('\\')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        if !matches!(
            bus.as_str(),
            "USB" | "HID" | "BTHENUM" | "BTHLEDEVICE" | "DISPLAY"
        ) {
            continue;
        }
        let mut device = 0;
        if unsafe { CM_Locate_DevNodeW(&mut device, id.as_ptr(), 0) } != CR_SUCCESS {
            continue;
        }
        let class = property(device, CM_DRP_CLASS)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(class.as_str(), "media" | "audioendpoint" | "softwaredevice") {
            continue;
        }
        let name = property(device, CM_DRP_FRIENDLYNAME)
            .filter(|name| !name.is_empty())
            .or_else(|| property(device, CM_DRP_DEVICEDESC));
        if let Some(label) = name.as_deref().and_then(catalog::physical_name) {
            names.insert(label.to_owned());
        }
    }
    Ok(names)
}

fn audio() -> io::Result<BTreeSet<String>> {
    let endpoints = crate::endpoint_alias::active_endpoint_names()
        .ok_or_else(|| io::Error::other("Windows audio endpoint inventory unavailable"))?;
    Ok(endpoints
        .into_iter()
        .filter_map(|(name, direction)| {
            catalog::audio(&name).map(|label| format!("{label} \u{00b7} {direction}"))
        })
        .collect())
}

fn profile(path: &Path) -> io::Result<Option<Value>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_STEAM_PROFILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STEAM_PROFILE {
        return Err(io::Error::other("SteamVR profile is too large"));
    }
    let content = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    Ok(Some(serde_json::from_slice(content)?))
}

fn saved(steam: &Path) -> io::Result<Vec<String>> {
    let config = steam.join("config");
    let mut saved = Vec::new();
    if let Some(profile) = profile(&config.join("steamvr.vrsettings"))? {
        if let Some(label) = profile["LastKnown"]["HMDModel"]
            .as_str()
            .and_then(catalog::model)
        {
            saved.push(label.to_owned());
        }
    }
    let directory = match fs::read_dir(config.join("lighthouse")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(catalog::counted(saved))
        }
        Err(error) => return Err(error),
    };
    for entry in directory {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(device) = profile(&path.join("config.json"))? {
            if matches!(
                device["device_class"].as_str(),
                Some("hmd" | "controller" | "generic_tracker" | "tracking_reference")
            ) {
                if let Some(label) = device["model_number"].as_str().and_then(catalog::model) {
                    saved.push(label.to_owned());
                }
            }
        }
    }
    Ok(catalog::counted(saved))
}

fn utc_now() -> String {
    let mut time: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetSystemTime(&mut time) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

fn tracked(executable: &Path, script: &Path) -> Option<Vec<String>> {
    let mut child = Command::new(executable)
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .ok()?;
    let timeout = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < timeout => thread::sleep(Duration::from_millis(40)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut bytes = Vec::new();
    child
        .stdout
        .take()?
        .take(8193)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 8192 {
        return None;
    }
    let labels: Vec<String> = serde_json::from_slice(&bytes).ok()?;
    (labels.len() <= 64 && labels.iter().all(|label| catalog::tracked_label(label)))
        .then_some(labels)
}

fn scan(executable: &Path, script: &Path, steam: &Path) -> Value {
    let mut warnings = Vec::new();
    let (connected, runtimes) = match processes().and_then(|processes| Ok((processes, physical()?)))
    {
        Ok((processes, names)) => {
            let runtimes = RUNTIMES
                .iter()
                .filter_map(|(process, label)| processes.contains(*process).then_some(*label))
                .collect::<BTreeSet<_>>();
            let connected = if processes.contains("vrserver") {
                match tracked(executable, script) {
                    Some(tracked) => catalog::merge_connected(names, tracked),
                    None => {
                        warnings.push("SteamVR tracked-device status unavailable.");
                        names.into_iter().collect()
                    }
                }
            } else {
                names.into_iter().collect()
            };
            (connected, runtimes)
        }
        Err(_) => {
            warnings.push("Windows hardware and runtime status unavailable.");
            (Vec::new(), BTreeSet::new())
        }
    };
    let audio = audio().unwrap_or_else(|_| {
        warnings.push("Windows audio endpoint status unavailable.");
        BTreeSet::new()
    });
    let saved = saved(steam).unwrap_or_else(|_| {
        warnings.push("Saved SteamVR profiles unavailable.");
        Vec::new()
    });
    json!({"connected":connected, "runtimes":runtimes, "audio":audio,
           "saved":saved, "warnings":warnings, "scannedAt":utc_now()})
}

struct Cached {
    checked: Instant,
    payload: Vec<u8>,
}

pub unsafe fn copy_result(data: &[u8], output: *mut u8, capacity: usize) -> usize {
    if output.is_null() || data.len() > capacity {
        return 0;
    }
    std::ptr::copy_nonoverlapping(data.as_ptr(), output, data.len());
    data.len()
}

#[no_mangle]
pub unsafe extern "C" fn lummix_vr_scan(
    input: *const u8,
    length: usize,
    output: *mut u8,
    capacity: usize,
) -> usize {
    if input.is_null() || length == 0 || length > 384_000 {
        return 0;
    }
    let Ok(paths) = serde_json::from_slice::<Value>(slice::from_raw_parts(input, length)) else {
        return 0;
    };
    let (Some(executable), Some(script)) = (paths["exe"].as_str(), paths["script"].as_str()) else {
        return 0;
    };
    static CACHE: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    let Ok(mut cache) = CACHE.get_or_init(|| Mutex::new(None)).lock() else {
        return 0;
    };
    let value = cache.get_or_insert_with(|| Cached {
        checked: Instant::now(),
        payload: Vec::new(),
    });
    if value.payload.is_empty() || value.checked.elapsed() > CACHE_TIME {
        value.payload = scan(Path::new(executable), Path::new(script), &steam_root())
            .to_string()
            .into_bytes();
        value.checked = Instant::now();
    }
    copy_result(&value.payload, output, capacity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_profiles_remain_distinct_from_present_inventory() {
        let root = env::temp_dir().join(format!(
            "lummix-vr-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = root.join("config");
        let lighthouse = config.join("lighthouse");
        fs::create_dir_all(lighthouse.join("private-serial")).unwrap();
        fs::create_dir_all(lighthouse.join("private-listener")).unwrap();
        let result = (|| {
            fs::write(
                config.join("steamvr.vrsettings"),
                br#"{"LastKnown":{"HMDModel":"PICO 4 private-user-label"}}"#,
            )?;
            fs::write(
                lighthouse.join("private-serial/config.json"),
                br#"{"device_class":"hmd","model_number":"PICO 4"}"#,
            )?;
            fs::write(
                lighthouse.join("private-listener/config.json"),
                br#"{"device_class":"display","model_number":"Valve Index"}"#,
            )?;
            saved(&root)
        })();
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(result.unwrap(), ["PICO 4 \u{00d7}2"]);
    }
}
