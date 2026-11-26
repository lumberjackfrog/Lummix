use crate::{
    cable_setup as cable, endpoint_alias, private_http, vr_discovery, vr_openvr, wrist_preview,
    xsoverlay, xsoverlay_http,
};
use serde_json::{json, Value};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::Ipv4Addr,
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    ptr::null,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE},
    System::Threading::CREATE_NO_WINDOW,
};

const MEMBERS: [(&str, usize, &str); 31] = include!("cable_members.in");
const BUSY: [&str; 3] = ["checking", "preparing", "awaiting-windows"];
const ALIAS_ERRORS: [&str; 6] = [
    "ok",
    "name_unavailable",
    "name_collision",
    "name_not_applied",
    "name_timed_out",
    "name_busy",
];

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn powershell(data: &Path, script: &str, folder: Option<&Path>) -> Result<Value, String> {
    let directory =
        PathBuf::from(env::var_os("SystemRoot").ok_or("Windows system directory is unavailable")?)
            .join("System32/WindowsPowerShell/v1.0");
    let source = fs::read_to_string(data.join(script)).map_err(display)?;
    let mut command = Command::new(directory.join("powershell.exe"));
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]);
    if let Some(folder) = folder {
        command
            .arg(format!("& {{\n{source}\n}} -Folder $env:LUM_CABLE_FOLDER"))
            .env("LUM_CABLE_FOLDER", folder);
    } else {
        command.arg(source);
    }
    command.env("PSModulePath", directory.join("Modules"));
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(display)?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Could not read Windows probe output")?;
    let drain = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(262_145).read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(40)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let bytes = drain
        .join()
        .map_err(|_| "Could not read Windows probe output")?
        .map_err(display)?;
    if !success || bytes.len() > 262_144 {
        return Err("Windows cable evidence is unavailable".into());
    }
    serde_json::from_slice(&bytes).map_err(display)
}

fn windows_pair(data: &Path) -> Value {
    powershell(data, "cable_setup_status.ps1", None)
        .ok()
        .and_then(cable::qualified_live)
        .unwrap_or_else(|| json!({}))
}

fn marker() -> PathBuf {
    PathBuf::from(
        env::var_os("LOCALAPPDATA")
            .or_else(|| env::var_os("USERPROFILE"))
            .unwrap_or_default(),
    )
    .join("Lummix/vb-cable-restart")
}
fn needs_restart(boot: &Value, clear_expired: bool) -> bool {
    let path = marker();
    let Some(pending) = fs::read(&path)
        .ok()
        .and_then(|data| serde_json::from_slice::<Value>(&data).ok())
    else {
        return false;
    };
    match cable::restart_pending(&json!({"pending":pending,"boot":boot})) {
        Some(true) => true,
        Some(false) if clear_expired => {
            let _ = fs::remove_file(path);
            false
        }
        _ => false,
    }
}

fn alias_call(kind: u32, input: &Value) -> Result<Value, usize> {
    let payload = input.to_string();
    endpoint_alias::run(kind, payload.as_bytes()).map_err(|error| error as usize)
}

fn capture_label(ids: &Value) -> Value {
    match alias_call(0, ids) {
        Ok(item) => json!({"captureName":item["name"],
            "captureAlias":if item["collision"]==true {"unavailable"} else if item["short"]=="Lummix" {"ready"} else {"original"}}),
        _ => json!({"captureName":null,"captureAlias":"unavailable"}),
    }
}

pub fn cable_pair(data: &Path) -> Value {
    let probe = windows_pair(data);
    if needs_restart(&probe["boot"], false) {
        return json!({"error":"cable_restart_needed"});
    }
    if probe["signed"] != true || probe["state"] != "ready" {
        return json!({});
    }
    let Some(mut pair) = probe["endpointIds"].as_object().cloned() else {
        return json!({});
    };
    if let Value::Object(label) = capture_label(&probe["endpointIds"]) {
        pair.extend(label);
    }
    Value::Object(pair)
}

fn endpoint_worker(data: &Path, id: &str) -> usize {
    let mut ids = cable_pair(data);
    ids["workerId"] = json!(id);
    alias_call(2, &ids).map(|_| 0).unwrap_or_else(|error| error)
}

pub fn ensure_alias(data: &Path) -> Value {
    let ids = cable_pair(data);
    let before = alias_call(0, &ids);
    let mut result =
        json!({"captureName":null,"captureAlias":"unavailable","aliasError":"name_unavailable"});
    let Ok(before) = before else {
        return result;
    };
    result["captureName"] = before["name"].clone();
    if before["short"] == "Lummix" && before["collision"] != true {
        return json!({"captureName":before["name"],"captureAlias":"ready","aliasError":null});
    }
    if before["short"] != "Lummix" {
        result["captureAlias"] = json!("original");
    }
    if before["collision"] == true {
        result["aliasError"] = json!("name_collision");
        return result;
    }
    let Some(id) = before["id"].as_str().filter(|id| {
        !id.is_empty()
            && id
                .bytes()
                .all(|ch| ch.is_ascii_hexdigit() || b"{}-.".contains(&ch))
    }) else {
        return result;
    };
    let Ok(exe) = env::current_exe() else {
        return result;
    };
    let exe16: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
    let cmd: Vec<u16> = format!("\"{}\" name-cable-worker {id}", exe.display())
        .encode_utf16()
        .collect();
    let mut code = 0;
    let status = unsafe {
        endpoint_alias::lummix_cable_run_private(
            exe16.as_ptr(),
            cmd.as_ptr(),
            cmd.len(),
            22_000,
            &mut code,
        )
    };
    let error = if status == 1460 {
        "name_timed_out"
    } else if status != 0 {
        "name_unavailable"
    } else if code == 0 {
        "name_not_applied"
    } else {
        ALIAS_ERRORS
            .get(code as usize)
            .copied()
            .unwrap_or("name_not_applied")
    };
    let deadline =
        Instant::now() + Duration::from_secs(if status == 0 && code == 0 { 1 } else { 0 });
    loop {
        if let Ok(after) = alias_call(0, &ids) {
            if after["id"] == before["id"] && after["short"] == "Lummix" {
                return json!({"captureName":after["name"],"captureAlias":"ready","aliasError":null});
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    result["aliasError"] = json!(error);
    result
}

fn device_scan() -> Value {
    let Ok(exe) = env::current_exe() else {
        return json!({});
    };
    serde_json::from_slice(&vr_discovery::cached_scan(
        &exe,
        &[OsStr::new("openvr-devices")],
    ))
    .unwrap_or_else(|_| json!({}))
}

pub fn connected_headsets() -> String {
    device_scan()["connected"]
        .as_array()
        .map_or_else(String::new, |items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        })
}

struct Bundle {
    folder: PathBuf,
    locks: Vec<File>,
}
impl Drop for Bundle {
    fn drop(&mut self) {
        self.locks.clear();
        let _ = fs::remove_dir_all(&self.folder);
    }
}

fn sha(data: &[u8]) -> Result<String, String> {
    let mut digest = [0u8; 32];
    let status = unsafe {
        BCryptHash(
            BCRYPT_SHA256_ALG_HANDLE,
            null(),
            0,
            data.as_ptr(),
            data.len() as u32,
            digest.as_mut_ptr(),
            32,
        )
    };
    if status != 0 {
        return Err("Windows could not hash the vendor package".into());
    }
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn exact_members(source: &Path) -> Result<(), String> {
    let mut count = 0;
    for entry in fs::read_dir(source).map_err(display)? {
        let entry = entry.map_err(display)?;
        if !MEMBERS
            .iter()
            .any(|(name, _, _)| entry.file_name() == *name)
            || !entry.file_type().map_err(display)?.is_file()
        {
            return Err("Unexpected VB-CABLE package member".into());
        }
        count += 1;
    }
    if count != MEMBERS.len() {
        return Err("Incomplete VB-CABLE package".into());
    }
    Ok(())
}

fn member(source: &Path, name: &str, size: usize, digest: &str) -> Result<Vec<u8>, String> {
    let file = File::open(source.join(name)).map_err(display)?;
    let mut bytes = Vec::new();
    file.take(size as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(display)?;
    if bytes.len() != size || sha(&bytes)? != digest {
        return Err("VB-CABLE package byte verification failed".into());
    }
    Ok(bytes)
}

fn verify(data: &Path, folder: &Path) -> Result<Value, String> {
    let proof = powershell(data, "cable_setup_install.ps1", Some(folder))?;
    if !cable::trusted_signatures(&proof) {
        return Err("VB-AUDIO installer or Microsoft driver catalog signature failed".into());
    }
    Ok(proof)
}

fn prepare(data: &Path) -> Result<Bundle, String> {
    let mut archive = Vec::new();
    File::open(data.join("VBCABLE_Driver_Pack46.zip"))
        .map_err(display)?
        .take(1_324_681)
        .read_to_end(&mut archive)
        .map_err(display)?;
    if !unsafe { cable::lummix_cable_pinned_bytes(false, archive.as_ptr(), archive.len()) } {
        return Err("The VB-Audio archive does not match the reviewed package".into());
    }
    let source = data.join("VBCABLE");
    exact_members(&source)?;
    let suffix = private_http::token().ok_or("Windows could not allocate vendor staging")?;
    let folder = env::temp_dir().join(format!("Lummix-VBCABLE-{suffix}"));
    fs::create_dir(&folder).map_err(display)?;
    let mut bundle = Bundle {
        folder,
        locks: Vec::new(),
    };
    for (name, size, expected) in MEMBERS {
        let bytes = member(&source, name, size, expected)?;
        let path = bundle.folder.join(name);
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(display)?;
            file.write_all(&bytes).map_err(display)?;
            file.sync_all().map_err(display)?;
        }
        let mut locked = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .map_err(display)?;
        let mut actual = Vec::new();
        (&mut locked)
            .take(size as u64 + 1)
            .read_to_end(&mut actual)
            .map_err(display)?;
        if actual != bytes {
            return Err("Vendor package member changed while staging".into());
        }
        bundle.locks.push(locked);
    }
    exact_members(&bundle.folder)?;
    verify(data, &bundle.folder)?;
    Ok(bundle)
}

fn phase(state: &str) -> Value {
    let label=match state {
        "ready"=>"VB-Audio’s signed VB-CABLE playback and microphone endpoints are ready.",
        "missing"=>"Installing VB-CABLE requires Windows administrator approval and a restart.",
        "partial"=>"An existing VB-CABLE installation was found without a verified active pair. If you just installed it, restart Windows.",
        "unavailable"=>"Windows could not verify the audio driver and both endpoints.",
        "preparing"=>"Preparing and verifying the original VB-Audio package.",
        "awaiting-windows"=>"Approve the Windows administrator prompt for VB-Audio to continue.",
        "cancelled"=>"Windows approval was declined.",
        "failed"=>"Setup could not be verified. No additional installation was attempted.",
        "restart-needed"=>"Restart Windows, then reopen Lummix and choose Set up Lummix to start the mix.",
        _=>"Checking VB-CABLE.",
    };
    json!({"state":state,"ready":state=="ready","restartMayBeNeeded":matches!(state,"partial"|"restart-needed"),"message":label,"endpoints":null})
}

fn current(data: &Path) -> (Value, Value) {
    let raw = windows_pair(data);
    let boot = raw["boot"].clone();
    if needs_restart(&boot, true) {
        return (phase("restart-needed"), boot);
    }
    let state = raw["state"].as_str().unwrap_or("unavailable");
    let mut value = phase(state);
    if raw.get("endpointIds").is_some() {
        if let Value::Object(label) = capture_label(&raw["endpointIds"]) {
            if let Some(fields) = value.as_object_mut() {
                fields.extend(label);
            }
        }
    }
    if state == "ready" {
        value["endpoints"] = alias_call(1, &raw["endpointIds"]).unwrap_or(Value::Null);
    }
    (value, boot)
}

fn write_marker(path: &Path, boot: &Value) -> Result<(), String> {
    let parent = path.parent().ok_or("Restart marker has no directory")?;
    fs::create_dir_all(parent).map_err(display)?;
    let name = private_http::token().ok_or("Could not allocate restart marker")?;
    let temp = parent.join(format!(".lummix-restart-{name}"));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(display)?;
    let result = file
        .write_all(boot.to_string().as_bytes())
        .and_then(|_| file.sync_all());
    drop(file);
    let result = result
        .and_then(|_| fs::rename(&temp, path))
        .map_err(display);
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

struct SetupState {
    value: Option<Value>,
    checked: Instant,
    restart: bool,
}
struct Controller {
    data: PathBuf,
    state: Mutex<SetupState>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl Controller {
    fn new(data: &Path) -> Arc<Self> {
        Arc::new(Self {
            data: data.to_owned(),
            state: Mutex::new(SetupState {
                value: None,
                checked: Instant::now(),
                restart: false,
            }),
            worker: Mutex::new(None),
        })
    }
    fn remember(&self, value: Value) {
        if let Ok(mut state) = self.state.lock() {
            state.value = Some(value);
            state.checked = Instant::now();
        }
    }
    fn detect(&self) -> (Value, Value) {
        if self.state.lock().is_ok_and(|state| state.restart) {
            return (phase("restart-needed"), Value::Null);
        }
        current(&self.data)
    }
    fn status(&self) -> Value {
        let Ok(mut state) = self.state.lock() else {
            return phase("unavailable");
        };
        if state.restart {
            return phase("restart-needed");
        }
        if let Some(value) = &state.value {
            if BUSY.contains(&value["state"].as_str().unwrap_or(""))
                || state.checked.elapsed() <= Duration::from_secs(8)
            {
                return value.clone();
            }
        }
        let (mut fresh, _) = current(&self.data);
        if matches!(fresh["state"].as_str(), Some("missing" | "partial")) {
            if let Some(previous) = &state.value {
                if matches!(previous["state"].as_str(), Some("failed" | "cancelled")) {
                    fresh = previous.clone();
                }
            }
        }
        state.value = Some(fresh.clone());
        state.checked = Instant::now();
        fresh
    }
    fn begin(self: &Arc<Self>) -> Value {
        let Ok(mut worker) = self.worker.lock() else {
            return phase("unavailable");
        };
        if worker.as_ref().is_some_and(|worker| !worker.is_finished()) {
            return self.status();
        }
        if let Some(previous) = worker.take() {
            let _ = previous.join();
        }
        let (value, _) = self.detect();
        if value["state"] != "missing" {
            self.remember(value.clone());
            return value;
        }
        let preparing = phase("preparing");
        self.remember(preparing.clone());
        let own = self.clone();
        match thread::Builder::new()
            .name("Lummix VB-CABLE setup".into())
            .spawn(move || own.work())
        {
            Ok(next) => {
                *worker = Some(next);
                preparing
            }
            Err(_) => {
                let failed = phase("failed");
                self.remember(failed.clone());
                failed
            }
        }
    }
    fn work(&self) {
        let path = marker();
        let mut marked = false;
        let result = (|| -> Result<Value, String> {
            let bundle = prepare(&self.data)?;
            let (now, boot) = self.detect();
            if now["state"] != "missing" {
                return Ok(now);
            }
            if boot
                .as_object()
                .is_some_and(|fields| fields.values().any(|value| !value.is_null()))
            {
                write_marker(&path, &boot)?;
                marked = true;
            }
            self.remember(phase("awaiting-windows"));
            exact_members(&bundle.folder)?;
            let proof = verify(&self.data, &bundle.folder)?;
            let code = cable::start_installer(bundle.folder.clone(), &proof);
            if code == u32::MAX {
                return Err("Windows could not start the signed vendor installer".into());
            }
            let mut outcome = cable::lummix_cable_install_result(code, false);
            if outcome == 2 {
                let ready = windows_pair(&self.data)["state"] == "ready";
                outcome = cable::lummix_cable_install_result(code, ready);
            }
            Ok(phase(match outcome {
                0 => "cancelled",
                1 => "restart-needed",
                _ => "failed",
            }))
        })()
        .unwrap_or_else(|_| phase("failed"));
        let restart = result["state"] == "restart-needed";
        if restart {
            if let Ok(mut state) = self.state.lock() {
                state.restart = true;
            }
        } else if marked {
            let _ = fs::remove_file(path);
        }
        self.remember(result);
    }
    fn join(&self) {
        if let Some(worker) = self.worker.lock().ok().and_then(|mut worker| worker.take()) {
            let _ = worker.join();
        }
    }
}

fn qa() -> bool {
    env::var_os("LUMMIX_QA_LOOPBACK").as_deref() == Some(OsStr::new("2"))
}

fn start_addon_on(data: &Path, qa: bool) -> Option<private_http::Listener> {
    let data = data.to_owned();
    xsoverlay_http::open_native(
        data.clone(),
        41839,
        41838,
        move |action| {
            Some(xsoverlay::addon(
                xsoverlay::discover().as_deref(),
                &data,
                action,
            ))
        },
        Ipv4Addr::new(127, 0, 0, if qa { 2 } else { 1 }),
    )
    .map(|(listener, _)| listener)
}

pub fn start_addon(data: &Path) -> Result<Option<private_http::Listener>, String> {
    Ok(start_addon_on(data, false))
}

fn wait_for_parent(linked: bool) {
    if linked {
        let _ = io::copy(&mut io::stdin().lock(), &mut io::sink());
    } else {
        loop {
            thread::park();
        }
    }
}

pub fn run_mode(mode: &str, args: &[OsString], data: &Path) -> Result<bool, String> {
    let linked = args.iter().any(|argument| argument == "--parent-stdin");
    match mode {
        "wrist" => {
            let controller = Controller::new(data);
            let state = controller.clone();
            let config = PathBuf::from(env::var_os("USERPROFILE").unwrap_or_default())
                .join("AppData/LocalLow/Xiexe/XSOverlay/config/overlaySettings.json");
            let (listener, _) = wrist_preview::open_native(
                data.to_owned(),
                xsoverlay::discover(),
                config,
                41836,
                qa(),
                move |action| {
                    Some(match action {
                        0 => state.status(),
                        1 => state.begin(),
                        2 => device_scan(),
                        _ => return None,
                    })
                },
            )
            .ok_or("The native wrist preview listener could not start")?;
            wait_for_parent(linked);
            drop(listener);
            controller.join();
        }
        "addon" => {
            let _listener = start_addon_on(data, qa())
                .ok_or("The native XSOverlay listener could not start")?;
            wait_for_parent(linked);
        }
        "remove-addon" => {
            let _ = xsoverlay::addon(xsoverlay::discover().as_deref(), data, 2);
        }
        "name-cable" => {
            let result = ensure_alias(data);
            println!("{result}");
            if result["captureAlias"] != "ready" {
                std::process::exit(1);
            }
        }
        "name-cable-worker" => {
            let id = args
                .first()
                .and_then(|value| value.to_str())
                .ok_or("Missing cable identifier")?;
            std::process::exit(endpoint_worker(data, id) as i32);
        }
        "verify-vendor" => {
            let _bundle = prepare(data)?;
            println!("Original signed VB-CABLE archive and catalogs verified.");
        }
        "detect-devices" | "devices" => println!("{}", device_scan()),
        "openvr-devices" => {
            let Some(found) = vr_openvr::read_devices() else {
                std::process::exit(1);
            };
            println!("{}", json!(found));
        }
        value
            if Path::new(value)
                .file_name()
                .is_some_and(|name| name == "detect_openvr.py") =>
        {
            let Some(found) = vr_openvr::read_devices() else {
                std::process::exit(1);
            };
            println!("{}", json!(found));
        }
        _ => return Ok(false),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vendor_member_pins_are_complete_unique_and_match_reviewed_size_constraints() {
        use std::collections::BTreeSet;
        let names = MEMBERS
            .iter()
            .map(|row| row.0.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), MEMBERS.len());
        assert!(
            names.contains("vbcable_setup_x64.exe") && names.contains("vbaudio_cable64_win10.cat")
        );
        assert!(MEMBERS
            .iter()
            .all(|(name, _, hash)| !name.contains(['/', '\\']) && hash.len() == 64));
        assert!(MEMBERS.iter().map(|x| x.1).sum::<usize>() < 10_000_000);
    }
    #[test]
    fn setup_offline_shape_keeps_controls_disabled() {
        assert_eq!(phase("ready")["ready"], true);
        assert_eq!(phase("restart-needed")["ready"], false);
        assert_eq!(phase("preparing")["endpoints"], Value::Null);
    }
}
