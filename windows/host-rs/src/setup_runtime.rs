use crate::{
    cable_setup as cable, endpoint_alias, private_http, sign_in, vr_discovery, vr_openvr,
    wrist_preview, xsoverlay, xsoverlay_http,
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
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
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
// Older installs also use the driver marker. A normal status check may consume
// that marker but must never consume the user's request to finish setup.
fn resume_path() -> PathBuf {
    marker().with_file_name("setup-resume.json")
}

fn read_resume() -> Option<Value> {
    let mut bytes = Vec::new();
    File::open(resume_path())
        .ok()?
        .take(4097)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= 4096)
        .then(|| serde_json::from_slice(&bytes).ok())
        .flatten()
}

#[derive(Debug, PartialEq)]
enum ResumeBoot {
    Same,
    Changed,
    Unknown,
}

fn resume_boot(pending: &Value, boot: &Value) -> ResumeBoot {
    let comparable = pending["event"]
        .as_str()
        .zip(boot["event"].as_str())
        .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
        || pending["ticks"]
            .as_i64()
            .zip(boot["ticks"].as_i64())
            .is_some();
    if !comparable {
        return ResumeBoot::Unknown;
    }
    match cable::restart_pending(&json!({"pending":pending,"boot":boot})) {
        Some(false) => ResumeBoot::Changed,
        Some(true) => ResumeBoot::Same,
        None => ResumeBoot::Unknown,
    }
}

pub fn clear_resume(exe: &Path) {
    let _ = sign_in::resume_registration(exe, false);
    let _ = fs::remove_file(resume_path());
}

pub fn resume_login(exe: &Path, data: &Path) -> bool {
    let Some(pending) = read_resume() else {
        clear_resume(exe);
        return false;
    };
    // A sign-out without rebooting must neither open the mixer nor lose the intent.
    resume_boot(&pending, &windows_pair(data)["boot"]) != ResumeBoot::Same
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
    root: PathBuf,
    folder: PathBuf,
    locks: Vec<File>,
}
impl Drop for Bundle {
    fn drop(&mut self) {
        self.locks.clear();
        let _ = fs::remove_dir_all(&self.root);
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

fn verify(data: &Path, folder: &Path) -> Result<Value, String> {
    let proof = powershell(data, "cable_setup_install.ps1", Some(folder))?;
    if !cable::trusted_signatures(&proof) {
        return Err("VB-AUDIO installer or Microsoft driver catalog signature failed".into());
    }
    Ok(proof)
}

fn prepare(data: &Path) -> Result<Bundle, String> {
    let bytes = crate::release_feed::vendor_archive()
        .map_err(|_| "Could not download the official VB-CABLE driver")?;
    if !unsafe { cable::lummix_cable_pinned_bytes(false, bytes.as_ptr(), bytes.len()) } {
        return Err("The VB-Audio download differs from the reviewed driver".into());
    }
    let suffix = private_http::token().ok_or("Could not allocate vendor staging")?;
    let root = env::temp_dir().join(format!("Lummix-VBCABLE-{suffix}"));
    fs::create_dir(&root).map_err(display)?;
    let folder = root.join("driver");
    let mut bundle = Bundle {
        root,
        folder,
        locks: Vec::new(),
    };
    fs::create_dir(&bundle.folder).map_err(display)?;
    let archive = bundle.root.join("vendor.zip");
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&archive)
        .map_err(display)?;
    output.write_all(&bytes).map_err(display)?;
    drop(output);
    let mut lock = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&archive)
        .map_err(display)?;
    let mut saved = Vec::new();
    (&mut lock)
        .take((bytes.len() + 1) as u64)
        .read_to_end(&mut saved)
        .map_err(display)?;
    if saved != bytes {
        return Err("Driver archive changed during staging".into());
    }
    bundle.locks.push(lock);
    powershell(data, "cable_setup_unpack.ps1", Some(&bundle.folder))?;
    exact_members(&bundle.folder)?;
    for (name, size, digest) in MEMBERS {
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(bundle.folder.join(name))
            .map_err(display)?;
        let mut contents = Vec::new();
        (&mut file)
            .take(size as u64 + 1)
            .read_to_end(&mut contents)
            .map_err(display)?;
        if contents.len() != size || sha(&contents)? != digest {
            return Err("Extracted VB-CABLE member differs from the reviewed driver".into());
        }
        bundle.locks.push(file);
    }
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
        "restart-needed"=>"Restart Windows to finish setting up the virtual microphone.",
        _=>"Checking VB-CABLE.",
    };
    let mut info = json!({"state":state,"ready":state=="ready","restartMayBeNeeded":matches!(state,"partial"|"restart-needed"),"message":label,"endpoints":null});
    if state == "restart-needed" {
        info["resumeAtSignIn"] =
            json!(env::current_exe().is_ok_and(|exe| sign_in::resume_enabled(&exe)));
    }
    info
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
    resume: Value,
}
struct Controller {
    data: PathBuf,
    state: Mutex<SetupState>,
    worker: Mutex<Option<JoinHandle<()>>>,
    closing: AtomicBool,
}
impl Controller {
    fn new(data: &Path) -> Arc<Self> {
        let pending = read_resume();
        let controller = Arc::new(Self {
            data: data.to_owned(),
            state: Mutex::new(SetupState {
                value: None,
                checked: Instant::now(),
                restart: false,
                resume: json!({"state": if pending.is_some() { "checking" } else { "none" }}),
            }),
            worker: Mutex::new(None),
            closing: AtomicBool::new(false),
        });
        if let Some(pending) = pending {
            let own = controller.clone();
            match thread::Builder::new()
                .name("Lummix setup continuation".into())
                .spawn(move || own.resume(pending))
            {
                Ok(handle) => *controller.worker.lock().unwrap() = Some(handle),
                Err(_) => controller.set_resume(json!({"state":"attention"})),
            }
        }
        controller
    }
    fn set_resume(&self, result: Value) {
        if let Ok(mut state) = self.state.lock() {
            state.resume = result;
        }
    }
    fn resume(&self, pending: Value) {
        let (mut detected, boot) = current(&self.data);
        let timing = resume_boot(&pending, &boot);
        if timing == ResumeBoot::Same {
            self.set_resume(json!({"state":"none"}));
            return;
        }
        // Audio endpoints can arrive later than Explorer at logon. Only detect and
        // name the cable here. Opening the microphone remains a mixer button action.
        let deadline = Instant::now() + Duration::from_secs(20);
        while timing == ResumeBoot::Changed
            && detected["state"] != "ready"
            && Instant::now() < deadline
        {
            for _ in 0..8 {
                if self.closing.load(Ordering::Acquire) {
                    return;
                }
                thread::sleep(Duration::from_millis(250));
            }
            detected = current(&self.data).0;
        }
        if self.closing.load(Ordering::Acquire) {
            return;
        }
        let result = if timing == ResumeBoot::Changed && detected["state"] == "ready" {
            let label = ensure_alias(&self.data);
            json!({"state":"ready", "captureName":label["captureName"].as_str().or(detected["endpoints"]["capture"].as_str())})
        } else {
            json!({"state":"attention"})
        };
        if let Ok(exe) = env::current_exe() {
            clear_resume(&exe);
        }
        self.remember(detected);
        self.set_resume(result);
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
            exact_members(&bundle.folder)?;
            let proof = verify(&self.data, &bundle.folder)?;
            if marked {
                if let Ok(exe) = env::current_exe() {
                    // Arm before UAC: the vendor could reboot the computer itself.
                    if write_marker(&resume_path(), &boot).is_ok() {
                        let _ = sign_in::resume_registration(&exe, true);
                    }
                }
            }
            self.remember(phase("awaiting-windows"));
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
            if let Ok(exe) = env::current_exe() {
                clear_resume(&exe);
            }
            let _ = fs::remove_file(path);
        }
        self.remember(result);
    }
    fn join(&self) {
        self.closing.store(true, Ordering::Release);
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
                        3 => state.state.lock().ok()?.resume.clone(),
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
    fn a_signout_is_not_a_reboot_and_missing_boot_evidence_is_explicit() {
        let before = json!({"event":"previous", "ticks":100_000_000});
        assert_eq!(resume_boot(&before, &before), ResumeBoot::Same);
        assert_eq!(
            resume_boot(&before, &json!({"event":"next", "ticks":100_000_000})),
            ResumeBoot::Changed
        );
        assert_eq!(
            resume_boot(&before, &json!({"ticks":110_000_000})),
            ResumeBoot::Same
        );
        assert_eq!(
            resume_boot(&before, &json!({"ticks":900_000_000})),
            ResumeBoot::Changed
        );
        assert_eq!(resume_boot(&before, &json!({})), ResumeBoot::Unknown);
    }
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
