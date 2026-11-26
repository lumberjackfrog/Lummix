use serde_json::{json, Value};
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::OpenOptions,
    io::{self, Read},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
    },
    path::PathBuf,
    ptr::{null, null_mut},
    slice,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED, HANDLE, WAIT_OBJECT_0},
    Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE},
    System::{
        Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE},
        Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE},
    },
    UI::{
        Shell::{
            ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
            SHELLEXECUTEINFOW,
        },
        WindowsAndMessaging::SW_SHOWNORMAL,
    },
};
use winreg::{
    enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY},
    RegKey,
};

const ARCHIVE_SIZE: usize = 1_324_680;
const ARCHIVE_HASH: [u8; 32] = [
    0xbc, 0x44, 0xe8, 0x97, 0x41, 0x91, 0xa5, 0xa2, 0x42, 0x7d, 0x6c, 0x97, 0x9e, 0x2d, 0xab, 0x08,
    0xdb, 0xb0, 0x99, 0x25, 0xf3, 0xf4, 0x35, 0xdf, 0xa2, 0xf0, 0xba, 0x3e, 0x45, 0xb6, 0xeb, 0x90,
];
const SETUP_HASH: [u8; 32] = [
    0x73, 0x4c, 0x35, 0xdf, 0xa6, 0xd9, 0x8f, 0x48, 0x78, 0x2a, 0x45, 0x16, 0x33, 0xce, 0xb4, 0x71,
    0x16, 0x6e, 0xc7, 0x0d, 0x60, 0x48, 0x2f, 0xd8, 0x9a, 0x11, 0x23, 0xd0, 0xee, 0x3c, 0x4f, 0x41,
];
const DRIVER: &str = "VB-Audio Virtual Cable";
const PROVIDER: &str = "VB-Audio Software";
const HARDWARE: &str = "VBAudioVACWDM";
const PUBLISHER: &str = "Microsoft Windows Hardware Compatibility Publisher";
const SETUP_THUMBPRINT: &str = "A77952D93229D0EC36E2543081EEA7D125732B9C";
const JSON_LIMIT: usize = 1024 * 1024;

fn eq(value: &Value, identity: &str) -> bool {
    value
        .as_str()
        .is_some_and(|text| text.eq_ignore_ascii_case(identity))
}

fn qualified_pair(raw: &Value) -> Option<Value> {
    let drivers = raw["drivers"].as_array()?;
    let mut present = false;
    let mut signed = HashSet::new();
    for driver in drivers {
        if eq(&driver["name"], DRIVER)
            && eq(&driver["provider"], PROVIDER)
            && eq(&driver["hardware"], HARDWARE)
        {
            present = true;
            if driver["signed"].as_bool() == Some(true) && eq(&driver["signer"], PUBLISHER) {
                if let Some(inf) = driver["inf"].as_str().filter(|inf| !inf.is_empty()) {
                    signed.insert(inf.to_ascii_lowercase());
                }
            }
        }
    }

    let mut ids = [Vec::new(), Vec::new()];
    for (index, flow, prefix) in [
        (0, "Render", "{0.0.0.00000000}."),
        (1, "Capture", "{0.0.1.00000000}."),
    ] {
        for endpoint in raw["endpoints"][flow].as_array()? {
            if endpoint["state"].as_u64() != Some(1)
                || !eq(&endpoint["hardware"], HARDWARE)
                || !eq(&endpoint["description"], DRIVER)
            {
                continue;
            }
            present = true;
            let Some(inf) = endpoint["inf"].as_str() else {
                continue;
            };
            if signed.contains(&inf.to_ascii_lowercase()) {
                let key = endpoint["key"].as_str()?;
                if guid_key(key) {
                    ids[index].push(format!("{prefix}{key}"));
                }
            }
        }
    }
    let render = !ids[0].is_empty();
    let capture = !ids[1].is_empty();
    let state = if render && capture {
        "ready"
    } else if present {
        "partial"
    } else {
        "missing"
    };
    Some(json!({
        "state": state,
        "signed": !signed.is_empty(),
        "capture": capture,
        "render": render,
        "endpointIds": {"Render":ids[0], "Capture":ids[1]},
        "boot":raw["boot"],
    }))
}

fn registry_endpoints() -> io::Result<Value> {
    let root = RegKey::predef(HKEY_LOCAL_MACHINE);
    let base = r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio";
    let mut flows = json!({});
    for flow in ["Render", "Capture"] {
        let bank =
            root.open_subkey_with_flags(format!(r"{base}\{flow}"), KEY_READ | KEY_WOW64_64KEY)?;
        let mut endpoints = Vec::new();
        for key in bank.enum_keys() {
            let key = key?;
            let device = match bank.open_subkey(&key) {
                Ok(device) => device,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let props = match device.open_subkey("Properties") {
                Ok(props) => props,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            // These MMDevices values are directly associated with the endpoint.
            // The final readiness decision also requires independent signed PnP evidence.
            endpoints.push(json!({
                "key": key,
                "state": device.get_value::<u32, _>("DeviceState").ok(),
                "hardware": props.get_value::<String, _>("{a8b865dd-2e3d-4094-ad97-e593a70c75d6},8").ok(),
                "inf": props.get_value::<String, _>("{a8b865dd-2e3d-4094-ad97-e593a70c75d6},5").ok(),
                "description": props.get_value::<String, _>("{b3f8fa53-0004-438e-9003-51a46e139bfc},6").ok(),
            }));
        }
        flows[flow] = json!(endpoints);
    }
    Ok(flows)
}

fn qualified_live(mut input: Value) -> Option<Value> {
    input
        .as_object_mut()?
        .insert("endpoints".into(), registry_endpoints().ok()?);
    qualified_pair(&input)
}

fn guid_key(key: &str) -> bool {
    key.len() == 38
        && key.bytes().enumerate().all(|(index, byte)| match index {
            0 => byte == b'{',
            37 => byte == b'}',
            9 | 14 | 19 | 24 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn valid_archive_layout(data: &Value) -> bool {
    let Some(entries) = data.as_array() else {
        return false;
    };
    let mut seen = HashSet::new();
    let mut total = 0;
    for entry in entries {
        let (Some(name), Some(size)) = (entry["name"].as_str(), entry["size"].as_u64()) else {
            return false;
        };
        let Some((stem, extension)) = name.split_once('.') else {
            return false;
        };
        if stem.is_empty()
            || !stem
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || !matches!(
                extension.to_ascii_lowercase().as_str(),
                "exe" | "inf" | "cat" | "sys" | "ico" | "txt"
            )
            || !seen.insert(name.to_ascii_lowercase())
            || size > 10_000_000 - total
        {
            return false;
        }
        total += size;
    }
    seen.contains("vbcable_setup_x64.exe") && seen.contains("vbaudio_cable64_win10.cat")
}

fn trusted_signatures(input: &Value) -> bool {
    input["installerStatus"] == "Valid"
        && eq(&input["installerThumbprint"], SETUP_THUMBPRINT)
        && input["catalogStatus"] == "Valid"
        && eq(&input["catalogSigner"], PUBLISHER)
}

fn restart_pending(input: &Value) -> Option<bool> {
    let pending = &input["pending"];
    let boot = &input["boot"];
    let old_event = pending["event"].as_str().filter(|value| !value.is_empty());
    let old_ticks = pending["ticks"].as_i64();
    if old_event.is_none() && old_ticks.is_none() {
        return None;
    }
    if let (Some(old), Some(new)) = (
        old_event,
        boot["event"].as_str().filter(|value| !value.is_empty()),
    ) {
        return Some(old == new);
    }
    Some(match (old_ticks, boot["ticks"].as_i64()) {
        (Some(old), Some(new)) => new.abs_diff(old) <= 20_000_000,
        _ => true,
    })
}

#[no_mangle]
pub unsafe extern "C" fn lummix_cable_check(
    operation: u32,
    data: *const u8,
    length: usize,
    output: *mut u8,
    capacity: usize,
) -> usize {
    if data.is_null() || output.is_null() || length > JSON_LIMIT {
        return 0;
    }
    let Ok(input) = serde_json::from_slice(slice::from_raw_parts(data, length)) else {
        return 0;
    };
    let result = match operation {
        0 => qualified_pair(&input),
        1 => Some(json!(valid_archive_layout(&input))),
        2 => Some(json!(trusted_signatures(&input))),
        3 => Some(json!(restart_pending(&input))),
        4 => qualified_live(input),
        _ => None,
    };
    let Some(data) = result.and_then(|value| serde_json::to_vec(&value).ok()) else {
        return 0;
    };
    if data.len() > capacity {
        return 0;
    }
    std::ptr::copy_nonoverlapping(data.as_ptr(), output, data.len());
    data.len()
}

#[no_mangle]
pub unsafe extern "C" fn lummix_cable_pinned_bytes(
    installer: bool,
    data: *const u8,
    length: usize,
) -> bool {
    if data.is_null()
        || (installer && length > 10_000_000)
        || (!installer && length != ARCHIVE_SIZE)
    {
        return false;
    }
    let mut hash = [0; 32];
    let status = BCryptHash(
        BCRYPT_SHA256_ALG_HANDLE,
        null(),
        0,
        data,
        length as u32,
        hash.as_mut_ptr(),
        32,
    );
    let expected = if installer { SETUP_HASH } else { ARCHIVE_HASH };
    status == 0 && hash == expected
}

#[no_mangle]
pub extern "C" fn lummix_cable_install_result(code: u32, ready: bool) -> u32 {
    match code {
        1223 => 0,
        0 | 1641 | 3010 => 1,
        _ if ready => 1,
        _ => 2,
    }
}

const LAUNCH_FAILED: u32 = u32::MAX;

struct Apartment;

impl Apartment {
    fn enter() -> Option<Self> {
        let hr = unsafe {
            CoInitializeEx(
                null_mut(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        (hr >= 0).then(|| Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct InstallerProcess(HANDLE);

impl Drop for InstallerProcess {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn start_installer(folder: PathBuf, proof: &Value) -> u32 {
    if !folder.is_absolute() || !trusted_signatures(proof) {
        return LAUNCH_FAILED;
    }
    let installer = folder.join("VBCABLE_Setup_x64.exe");
    // Keep the exact executable pinned against replacements until the real
    // vendor process exits. The caller retains the remaining package locks.
    let Ok(locked) = OpenOptions::new().read(true).share_mode(1).open(&installer) else {
        return LAUNCH_FAILED;
    };
    let mut bytes = Vec::new();
    if (&locked).take(10_000_001).read_to_end(&mut bytes).is_err()
        || !unsafe { lummix_cable_pinned_bytes(true, bytes.as_ptr(), bytes.len()) }
    {
        return LAUNCH_FAILED;
    }
    let Some(_apartment) = Apartment::enter() else {
        return LAUNCH_FAILED;
    };
    let file: Vec<u16> = installer.as_os_str().encode_wide().chain(Some(0)).collect();
    let directory: Vec<u16> = folder.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut run: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    run.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    run.fMask = SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI;
    run.lpVerb = windows_sys::w!("runas");
    run.lpFile = file.as_ptr();
    run.lpParameters = windows_sys::w!("-i -h");
    run.lpDirectory = directory.as_ptr();
    run.nShow = SW_SHOWNORMAL;
    if unsafe { ShellExecuteExW(&mut run) } == 0 {
        return if unsafe { GetLastError() } == ERROR_CANCELLED {
            ERROR_CANCELLED
        } else {
            LAUNCH_FAILED
        };
    }
    if run.hProcess.is_null() {
        return LAUNCH_FAILED;
    }
    let process = InstallerProcess(run.hProcess);
    // A driver installation must run to completion even if the main window closes.
    if unsafe { WaitForSingleObject(process.0, INFINITE) } != WAIT_OBJECT_0 {
        return LAUNCH_FAILED;
    }
    let mut code = LAUNCH_FAILED;
    if unsafe { GetExitCodeProcess(process.0, &mut code) } != 0 {
        code
    } else {
        LAUNCH_FAILED
    }
}

#[no_mangle]
pub unsafe extern "C" fn lummix_cable_launch(
    folder: *const u16,
    length: usize,
    proof: *const u8,
    proof_length: usize,
) -> u32 {
    if folder.is_null() || proof.is_null() || length == 0 || length > 32_767 || proof_length > 8192
    {
        return LAUNCH_FAILED;
    }
    let folder = slice::from_raw_parts(folder, length);
    if folder.contains(&0) {
        return LAUNCH_FAILED;
    }
    let Ok(proof) = serde_json::from_slice(slice::from_raw_parts(proof, proof_length)) else {
        return LAUNCH_FAILED;
    };
    start_installer(PathBuf::from(OsString::from_wide(folder)), &proof)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_both_active_flows_to_reference_a_signed_vendor_inf() {
        let mut raw = json!({
            "drivers":[{"name":DRIVER,"provider":PROVIDER,"hardware":HARDWARE,"signed":true,"signer":PUBLISHER,"inf":"OEM64.INF"}],
            "endpoints":{
                "Render":[{"key":"{0848C65D-BEF4-4ADE-8A19-98A8E04D68B4}","state":1,
                  "description":DRIVER,"hardware":HARDWARE,"inf":"oem64.inf"}],
                "Capture":[{"key":"{E613A90E-8A45-406E-8041-5CEFBA4B21F1}","state":1,
                  "description":DRIVER,"hardware":HARDWARE,"inf":"oem63.inf"}],
            }
        });
        assert_eq!(qualified_pair(&raw).unwrap()["state"], "partial");
        raw["endpoints"]["Capture"][0]["inf"] = json!("oem64.inf");
        assert_eq!(qualified_pair(&raw).unwrap()["state"], "ready");
        raw["drivers"][0]["signer"] = json!("VB-Audio Software");
        assert_eq!(qualified_pair(&raw).unwrap()["state"], "partial");
        raw["endpoints"]["Capture"][0]["state"] = json!(2);
        raw["drivers"] = json!([]);
        raw["endpoints"]["Render"] = json!([]);
        assert_eq!(qualified_pair(&raw).unwrap()["state"], "missing");
    }

    #[test]
    fn rejects_unsafe_archive_paths_duplicates_and_untrusted_catalogs() {
        let layout = json!([
            {"name":"VBCABLE_Setup_x64.exe","size":12},
            {"name":"vbaudio_cable64_win10.cat","size":20}
        ]);
        assert!(valid_archive_layout(&layout));
        for name in [
            "../vb.txt",
            "VBCABLE_setup_x64.Exe",
            "vb.txt.exe",
            "/driver.inf",
        ] {
            let mut data = layout.clone();
            data.as_array_mut()
                .unwrap()
                .push(json!({"name":name,"size":1}));
            assert!(!valid_archive_layout(&data));
        }
        let mut sig = json!({"installerStatus":"Valid","installerThumbprint":SETUP_THUMBPRINT,
            "catalogStatus":"Valid","catalogSigner":PUBLISHER});
        assert!(trusted_signatures(&sig));
        sig["catalogStatus"] = json!("UnknownError");
        assert!(!trusted_signatures(&sig));
    }

    #[test]
    fn boot_record_is_authoritative_and_cancellation_wins() {
        let prior = json!({"event":"123:456","ticks":100000000});
        assert_eq!(
            restart_pending(&json!({"pending":prior,"boot":{"ticks":120000000}})),
            Some(true)
        );
        assert_eq!(
            restart_pending(&json!({"pending":prior,"boot":{"event":"234:456","ticks":100000000}})),
            Some(false)
        );
        assert_eq!(
            restart_pending(&json!({"pending":prior,"boot":null})),
            Some(true)
        );
        assert_eq!(
            restart_pending(&json!({"pending":{"ticks":true},"boot":null})),
            None
        );
        assert_eq!(lummix_cable_install_result(1223, true), 0);
        assert_eq!(lummix_cable_install_result(3010, false), 1);
        assert_eq!(lummix_cable_install_result(17, true), 1);
        assert_eq!(lummix_cable_install_result(17, false), 2);
    }
}
