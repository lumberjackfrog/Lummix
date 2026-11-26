use serde_json::{json, Value};
use std::slice;

use std::{
    collections::HashSet,
    env,
    fs::{self, Metadata, OpenOptions},
    io::{self, Read, Write},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::OnceLock,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE},
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, OPEN_EXISTING,
    },
};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

const BEGIN: &[u8] = b"<!-- Lummix wrist add-on BEGIN -->";
const END: &[u8] = b"<!-- Lummix wrist add-on END -->";
const BODY: &[u8] = b"</body>";
const WRIST_LIMIT: usize = 4 * 1024 * 1024;
const REQUIRED_LAYOUT: [&[u8]; 3] = [
    b"uiContainer",
    b"./_Shared/js/uiComponents.js",
    b"./_Shared/js/toolbar.js",
];

#[repr(C)]
#[derive(Default)]
pub struct Layout {
    start: usize,
    end: usize,
    insert: usize,
    compatible: u32,
}

fn matches<'a>(data: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    data.windows(needle.len())
        .enumerate()
        .filter_map(move |(index, window)| (window == needle).then_some(index))
}

fn inspect_layout(wrist: &[u8]) -> Result<Layout, u32> {
    let mut layout = Layout::default();
    let mut starts = matches(wrist, BEGIN);
    let mut ends = matches(wrist, END);
    let clean;
    let stock = match (starts.next(), ends.next()) {
        (None, None) => wrist,
        (Some(start), Some(end)) if starts.next().is_none() && ends.next().is_none() => {
            if end < start {
                return Err(2);
            }
            layout.start = start;
            layout.end = end + END.len();
            clean = [&wrist[..layout.start], &wrist[layout.end..]].concat();
            &clean
        }
        _ => return Err(1),
    };

    let mut closing_tags = stock
        .windows(BODY.len())
        .enumerate()
        .filter_map(|(index, bytes)| bytes.eq_ignore_ascii_case(BODY).then_some(index));
    if let Some(insert) = closing_tags.next() {
        layout.insert = insert;
        layout.compatible = u32::from(
            closing_tags.next().is_none()
                && REQUIRED_LAYOUT
                    .iter()
                    .all(|marker| matches(stock, marker).next().is_some()),
        );
    }
    Ok(layout)
}

#[repr(u32)]
#[derive(Debug, PartialEq)]
enum Ownership {
    Uninstalled,
    Installed,
    Current,
    InvalidReceipt,
    EditedManifest,
    EditedIcon,
    EditedWrist,
    Unrecognized,
}

fn digest(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn ownership(facts: &Value) -> Ownership {
    let receipt = &facts["receipt"];
    let folder = facts["directory"].as_bool().unwrap_or(false);
    let same_files = facts["sameFiles"].as_bool().unwrap_or(false);
    let block = facts["block"].as_str();
    let same_block = block.is_some() && block == facts["expectedBlock"].as_str();
    if facts["hasReceipt"].as_bool() == Some(true) {
        let files = &receipt["files"];
        let proof_app = digest(&files["app.json"]);
        let proof_icon = digest(&files["icon.png"]);
        let proof_block = digest(&receipt["block"]);
        if receipt["owner"] != "Lummix"
            || receipt["format"].as_u64() != Some(1)
            || files.as_object().map(|files| files.len()) != Some(2)
            || proof_app.is_none()
            || proof_icon.is_none()
            || proof_block.is_none()
        {
            return Ownership::InvalidReceipt;
        }
        if facts["manifest"]
            .as_str()
            .is_some_and(|hash| Some(hash) != proof_app)
        {
            return Ownership::EditedManifest;
        }
        if facts["icon"]
            .as_str()
            .is_some_and(|hash| Some(hash) != proof_icon)
        {
            return Ownership::EditedIcon;
        }
        if block.is_some() && block != proof_block {
            return Ownership::EditedWrist;
        }
    } else if folder || (block.is_some() && !same_block) {
        return Ownership::Unrecognized;
    }
    if same_files && same_block {
        Ownership::Current
    } else if facts["hasReceipt"].as_bool() == Some(true) || block.is_some() {
        Ownership::Installed
    } else {
        Ownership::Uninstalled
    }
}

#[repr(u32)]
enum Disposition {
    Installable,
    Ready,
    ReadyReadOnly,
    Update,
    Unsupported,
    UnsupportedOwned,
    Blocked,
}

/// Convert compatibility, ownership and permission signals into install status.
fn disposition(compatible: bool, writable: bool, ownership: u32) -> u32 {
    let state = if !compatible {
        if ownership == Ownership::Uninstalled as u32 {
            Disposition::Unsupported
        } else {
            Disposition::UnsupportedOwned
        }
    } else if ownership == Ownership::Current as u32 {
        if writable {
            Disposition::Ready
        } else {
            Disposition::ReadyReadOnly
        }
    } else if !writable {
        Disposition::Blocked
    } else if ownership == Ownership::Installed as u32 {
        Disposition::Update
    } else {
        Disposition::Installable
    };
    state as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marker_offsets_preserve_other_bytes_and_ignore_closing_tags_in_the_owned_block() {
        let before = b"\xef\xbb\xbf<body id=uiContainer>./_Shared/js/uiComponents.js ./_Shared/js/toolbar.js";
        let after = b"<!-- vendor --></BODY>\r\n";
        let wrist = [before.as_slice(), BEGIN, b"</body>", END, after].concat();
        let layout = inspect_layout(&wrist).unwrap();
        assert_eq!(&wrist[..layout.start], before);
        assert_eq!(&wrist[layout.end..], after);
        assert_eq!(&after[layout.insert - before.len()..], b"</BODY>\r\n");
        assert_eq!(layout.compatible, 1);

        assert_eq!(inspect_layout(&[END, BEGIN].concat()).err(), Some(2));
        assert_eq!(inspect_layout(BEGIN).err(), Some(1));
    }

    #[test]
    fn receipt_requires_exact_owned_digests_and_can_restore_missing_files() {
        let hash = "a".repeat(64);
        let changed = "b".repeat(64);
        let mut facts = json!({
            "hasReceipt":true,
            "receipt":{"owner":"Lummix","format":1,"files":{"app.json":hash,"icon.png":hash},"block":hash},
            "manifest":hash, "icon":null, "block":hash, "expectedBlock":hash, "sameFiles":false
        });
        assert_eq!(ownership(&facts), Ownership::Installed);
        facts["icon"] = json!(changed);
        assert_eq!(ownership(&facts), Ownership::EditedIcon);
        facts["receipt"]["files"]["user.txt"] = json!(hash);
        assert_eq!(ownership(&facts), Ownership::InvalidReceipt);
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Prepare,
    Write,
    Restore,
    RemoveCreatedFolder,
    FinishRemove,
}

const SKIPPED: u32 = 0;
const APPLIED: u32 = 1;
const CONFLICT: u32 = 2;
const BLOCKED: u32 = 3;
const ROLLBACK_INCOMPLETE: u32 = 4;

fn transaction(install: bool, mut apply: impl FnMut(Phase, u32) -> u32) -> u32 {
    // Write the ownership receipt first during install. Remove it last so a
    // partially applied removal can still be recognized after a restart.
    let order = if install { [0, 1, 2, 3] } else { [3, 1, 2, 0] };
    let mut created = false;
    if install {
        match apply(Phase::Prepare, 0) {
            SKIPPED => {}
            APPLIED => created = true,
            CONFLICT => return CONFLICT,
            _ => return BLOCKED,
        }
    }
    let mut written = Vec::with_capacity(order.len());
    for slot in order {
        match apply(Phase::Write, slot) {
            SKIPPED => {}
            APPLIED => written.push(slot),
            failure => {
                let mut restored = true;
                for prior in written.into_iter().rev() {
                    restored &= apply(Phase::Restore, prior) == APPLIED;
                }
                if created {
                    restored &= apply(Phase::RemoveCreatedFolder, 0) == APPLIED;
                }
                return if !restored {
                    ROLLBACK_INCOMPLETE
                } else if failure == CONFLICT {
                    CONFLICT
                } else {
                    BLOCKED
                };
            }
        }
    }
    if !install {
        apply(Phase::FinishRemove, 0);
    }
    APPLIED
}

#[cfg(test)]
mod transaction_tests {
    use super::*;

    #[test]
    fn restores_every_write_in_reverse_order_after_a_conflict() {
        let mut operations = Vec::new();
        let result = transaction(true, |phase, slot| {
            operations.push((phase, slot));
            match (phase, slot) {
                (Phase::Write, 3) => CONFLICT,
                (Phase::Restore, 1) => CONFLICT,
                _ => APPLIED,
            }
        });
        assert_eq!(result, ROLLBACK_INCOMPLETE);
        assert_eq!(
            &operations[5..],
            &[
                (Phase::Restore, 2),
                (Phase::Restore, 1),
                (Phase::Restore, 0),
                (Phase::RemoveCreatedFolder, 0),
            ]
        );
    }
}

const NAME: &str = "Lummix";
const MANAGED: [&str; 3] = [".lummix-install.json", "app.json", "icon.png"];
const REPARSE: u32 = 0x400;
const STOCK_MANIFEST: &[u8] = br#"{
  "author": "Lummix",
  "name": "Lummix",
  "version": "v1.0",
  "description": "Lummix microphone and music controls.",
  "url": "http://127.0.0.1:41839/wrist",
  "resWidth": 720,
  "resHeight": 480,
  "iconColor": [
    0.68,
    0.52,
    0.78
  ],
  "defaultScale": 0.2,
  "canOpenMultiple": false,
  "canBeCurved": false
}
"#;

enum Problem {
    Conflict(&'static str),
    Io(io::Error),
}

impl From<io::Error> for Problem {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug)]
struct Directory(HANDLE);

impl Drop for Directory {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn open_directory(path: &Path, access: u32, share: u32) -> io::Result<Directory> {
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            share,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Directory(handle))
}

fn redirected() -> Problem {
    Problem::Conflict("An XSOverlay add-on path is redirected or has an unexpected file type.")
}

fn check_metadata(meta: &Metadata, directory: bool) -> Result<(), Problem> {
    if meta.file_attributes() & REPARSE != 0
        || (if directory {
            !meta.is_dir()
        } else {
            !meta.is_file()
        })
    {
        return Err(redirected());
    }
    Ok(())
}

fn regular(path: &Path, directory: bool) -> Result<bool, Problem> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            check_metadata(&meta, directory)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn guard_path(path: &Path, directory: bool) -> Result<bool, Problem> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(redirected());
    }
    for ancestor in path
        .ancestors()
        .skip(1)
        .filter(|entry| !entry.as_os_str().is_empty())
    {
        if !regular(ancestor, true)? {
            return Ok(false);
        }
    }
    regular(path, directory)
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, Problem> {
    if !guard_path(path, false)? {
        return Ok(None);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    check_metadata(&file.metadata()?, false)?;
    let mut bytes = Vec::new();
    file.take((WRIST_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > WRIST_LIMIT {
        return Err(Problem::Conflict(
            "An existing add-on file is too large to identify safely.",
        ));
    }
    Ok(Some(bytes))
}

fn sha(data: &[u8]) -> io::Result<String> {
    let mut digest = [0u8; 32];
    let result = unsafe {
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
    if result != 0 {
        return Err(io::Error::other("Could not hash XSOverlay asset"));
    }
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn optional_sha(bytes: Option<&[u8]>) -> io::Result<Option<String>> {
    bytes.map(sha).transpose()
}

type InstallFiles = [Vec<u8>; 3];
fn assets(root: &Path) -> Result<(InstallFiles, Vec<u8>), Problem> {
    let unavailable = || io::Error::other("Missing or invalid bundled XSOverlay payload");
    let script = read(&root.join("xsoverlay-live.js"))?.ok_or_else(unavailable)?;
    let icon = read(&root.join("xsoverlay-icon.png"))?.ok_or_else(unavailable)?;
    if !std::str::from_utf8(&script).is_ok_and(|text| !text.trim().is_empty())
        || script
            .windows(8)
            .any(|window| window.eq_ignore_ascii_case(b"</script"))
        || !icon.starts_with(b"\x89PNG\r\n\x1a\n")
    {
        return Err(unavailable().into());
    }
    let block = [BEGIN, b"\n<script>\n", &script, b"\n</script>\n", END].concat();
    // Keep the legacy sorted-key JSON bytes, including separators and final newline.
    let receipt = format!(
        r#"{{"block": "{}", "files": {{"app.json": "{}", "icon.png": "{}"}}, "format": 1, "owner": "Lummix"}}"#,
        sha(&block)?,
        sha(STOCK_MANIFEST)?,
        sha(&icon)?
    ) + "\n";
    Ok(([receipt.into_bytes(), STOCK_MANIFEST.to_vec(), icon], block))
}

fn status(state: &str, message: &str, removable: bool) -> Value {
    json!({"name":NAME,"state":state,"message":message,
           "canInstall":state=="installable","canUpdate":state=="update","canRemove":removable})
}

struct State {
    result: Value,
    snapshot: Option<Snapshot>,
}
impl State {
    fn only(state: &str, message: &str) -> Self {
        Self {
            result: status(state, message, false),
            snapshot: None,
        }
    }
}

struct Snapshot {
    appdir: PathBuf,
    paths: [PathBuf; 4],
    before: [Option<Vec<u8>>; 4],
    cleaned: Vec<u8>,
    insert: usize,
    files: InstallFiles,
    block: Vec<u8>,
}

fn inspect(root: &Path, data: &Path) -> Result<State, Problem> {
    if !root
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("Applications"))
        || !guard_path(root, true)?
    {
        return Ok(State::only(
            "unsupported",
            "The discovered XSOverlay installation has no compatible Applications directory.",
        ));
    }
    let root = root.canonicalize()?;
    let ui = root.join("_UI");
    let default = ui.join("Default");
    if !guard_path(&ui, true)? || !guard_path(&default, true)? {
        return Ok(State::only(
            "unsupported",
            "This XSOverlay installation has no supported wrist layout.",
        ));
    }
    let wrist_path = default.join("Wrist.html");
    let Some(wrist) = read(&wrist_path)? else {
        return Ok(State::only(
            "unsupported",
            "XSOverlay's wrist page is missing. Verify its files in Steam.",
        ));
    };
    let layout = inspect_layout(&wrist).map_err(|error| {
        Problem::Conflict(if error == 1 {
            "The Lummix markers in Wrist.html are incomplete or duplicated."
        } else {
            "The Lummix wrist markers are out of order."
        })
    })?;
    let clean = [&wrist[..layout.start], &wrist[layout.end..]].concat();
    let installed = (layout.end != 0).then(|| &wrist[layout.start..layout.end]);
    let (files, block) = assets(data)?;
    let appdir = root.join(NAME);
    let directory = guard_path(&appdir, true)?;
    let paths = [
        appdir.join(MANAGED[0]),
        appdir.join(MANAGED[1]),
        appdir.join(MANAGED[2]),
        wrist_path,
    ];
    let before = [
        read(&paths[0])?,
        read(&paths[1])?,
        read(&paths[2])?,
        Some(wrist.clone()),
    ];
    let receipt = before[0]
        .as_deref()
        .and_then(|data| serde_json::from_slice::<Value>(data).ok());
    let proof = ownership(&json!({
        "receipt":receipt, "hasReceipt":before[0].is_some(), "directory":directory,
        "manifest":optional_sha(before[1].as_deref())?, "icon":optional_sha(before[2].as_deref())?,
        "block":optional_sha(installed)?, "expectedBlock":sha(&block)?,
        "sameFiles":before[..3].iter().zip(&files).all(|(old,new)| old.as_deref()==Some(new.as_slice())),
    }));
    let conflict = match proof {
        Ownership::InvalidReceipt => Some("The Lummix ownership receipt is unrecognized. Existing files were preserved."),
        Ownership::EditedManifest => Some("The existing Lummix app.json was edited. Your copy was preserved."),
        Ownership::EditedIcon => Some("The existing Lummix icon.png was edited. Your copy was preserved."),
        Ownership::EditedWrist => Some("The installed Lummix wrist code was edited. Your copy was preserved."),
        Ownership::Unrecognized => Some("An unrecognized Lummix folder or wrist block already exists. Your files were preserved."),
        _ => None,
    };
    if let Some(message) = conflict {
        return Err(Problem::Conflict(message));
    }
    let writable = layout.compatible != 0
        && open_directory(&default, 2, 7).is_ok()
        && open_directory(
            if directory { &appdir } else { &root },
            if directory { 2 } else { 4 },
            7,
        )
        .is_ok();
    let state = disposition(layout.compatible != 0, writable, proof as u32);
    let result = match state {
        0 => status(
            "installable",
            "XSOverlay is ready for the wrist add-on.",
            false,
        ),
        1 => status("ready", "Installed in XSOverlay.", true),
        2 => status(
            "ready",
            "Installed. This Steam library is read-only.",
            false,
        ),
        3 => status(
            "update",
            "The add-on has an update or a missing file to restore.",
            true,
        ),
        4 | 5 => status(
            "unsupported",
            "This XSOverlay wrist layout is not supported.",
            state == 5,
        ),
        _ => {
            return Ok(State::only(
                "blocked",
                "Windows denied write access to the XSOverlay Steam library.",
            ))
        }
    };
    Ok(State {
        result,
        snapshot: Some(Snapshot {
            appdir,
            paths,
            before,
            cleaned: clean,
            insert: layout.insert,
            files,
            block,
        }),
    })
}

fn atomic_replace(path: &Path, data: Option<&[u8]>) -> Result<(), Problem> {
    if let Some(data) = data {
        let folder = path.parent().ok_or_else(redirected)?;
        let name = crate::private_http::token()
            .ok_or_else(|| io::Error::other("Could not allocate XSOverlay staging name"))?;
        let temp = folder.join(format!(".lummix-{name}.tmp"));
        let mut created = false;
        let result = (|| -> io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            created = true;
            file.write_all(data)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, path)?;
            Ok(())
        })();
        if created && result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn stable_directories(path: &Path) -> Result<Vec<Directory>, Problem> {
    let dirs = path
        .ancestors()
        .filter(|path| !path.as_os_str().is_empty())
        .collect::<Vec<_>>();
    let mut locks = Vec::new();
    for path in dirs.into_iter().rev() {
        if !regular(path, true)? {
            return Err(redirected());
        }
        locks.push(open_directory(path, 0, 3)?);
        if !regular(path, true)? {
            return Err(redirected());
        }
    }
    Ok(locks)
}

fn apply_snapshot(
    snapshot: Snapshot,
    install: bool,
    mut before_phase: impl FnMut(Phase, u32),
) -> u32 {
    let mut final_wrist = snapshot.cleaned;
    if install {
        final_wrist.splice(snapshot.insert..snapshot.insert, snapshot.block);
    }
    let [receipt, manifest, icon] = snapshot.files;
    let after = [
        install.then_some(receipt),
        install.then_some(manifest),
        install.then_some(icon),
        Some(final_wrist),
    ];
    transaction(install, |phase, slot| {
        before_phase(phase, slot);
        let change = || -> Result<u32, Problem> {
            match phase {
                Phase::Prepare => {
                    let root = snapshot.appdir.parent().ok_or_else(redirected)?;
                    let _guard = stable_directories(root)?;
                    if regular(&snapshot.appdir, true)? {
                        return Ok(SKIPPED);
                    }
                    fs::create_dir(&snapshot.appdir)?;
                    Ok(APPLIED)
                }
                Phase::RemoveCreatedFolder | Phase::FinishRemove => {
                    if guard_path(&snapshot.appdir, true)? {
                        fs::remove_dir(&snapshot.appdir)?;
                    }
                    Ok(APPLIED)
                }
                Phase::Write | Phase::Restore => {
                    let slot = slot as usize;
                    let path = &snapshot.paths[slot];
                    let prior = snapshot.before[slot].as_deref();
                    let next = after[slot].as_deref();
                    if phase == Phase::Write && prior == next {
                        return Ok(SKIPPED);
                    }
                    let _guard = stable_directories(path.parent().ok_or_else(redirected)?)?;
                    let (expected, replacement) = if phase == Phase::Write {
                        (prior, next)
                    } else {
                        (next, prior)
                    };
                    if read(path)?.as_deref() != expected {
                        return Ok(CONFLICT);
                    }
                    atomic_replace(path, replacement)?;
                    Ok(APPLIED)
                }
            }
        };
        match change() {
            Ok(outcome) => outcome,
            Err(Problem::Conflict(_)) => CONFLICT,
            Err(Problem::Io(_)) => BLOCKED,
        }
    })
}

fn inspected(root: &Path, data: &Path) -> State {
    match inspect(root, data) {
        Ok(state) => state,
        Err(Problem::Conflict(message)) => State::only("conflict", message),
        Err(Problem::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied => State::only(
            "blocked",
            "Windows denied access to the XSOverlay Steam library.",
        ),
        Err(Problem::Io(_)) => State::only(
            "unsupported",
            "The Lummix add-on files or XSOverlay installation could not be read.",
        ),
    }
}

struct AddonLock(Directory);
impl Drop for AddonLock {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::System::Threading::ReleaseMutex(self.0 .0) };
    }
}
fn lock(root: &Path) -> Option<AddonLock> {
    use windows_sys::Win32::{
        Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0},
        System::Threading::{CreateMutexW, WaitForSingleObject},
    };
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let digest = sha(canonical.to_string_lossy().to_lowercase().as_bytes()).ok()?;
    let name: Vec<u16> = format!("Local\\LummixXSOverlay-{digest}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mutex = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
    if mutex.is_null() {
        return None;
    }
    let mutex = Directory(mutex);
    match unsafe { WaitForSingleObject(mutex.0, 5000) } {
        WAIT_OBJECT_0 | WAIT_ABANDONED => Some(AddonLock(mutex)),
        _ => None,
    }
}

pub fn addon(root: Option<&Path>, data: &Path, action: u32) -> Value {
    let Some(root) = root else {
        return status(
            "missing",
            "XSOverlay was not found in a Steam library.",
            false,
        );
    };
    let Some(_guard) = lock(root) else {
        return status(
            "blocked",
            "Another Lummix operation is using the XSOverlay add-on.",
            false,
        );
    };
    let state = inspected(root, data);
    let allowed = match action {
        1 => state.result["canInstall"] == true || state.result["canUpdate"] == true,
        2 => state.result["canRemove"] == true,
        _ => false,
    };
    if !allowed {
        return state.result;
    }
    let Some(snapshot) = state.snapshot else {
        return state.result;
    };
    match apply_snapshot(snapshot, action == 1, |_, _| {}) {
        APPLIED => inspected(root, data).result,
        ROLLBACK_INCOMPLETE => status(
            "conflict",
            "The change stopped during rollback. Check your XSOverlay files before retrying.",
            false,
        ),
        outcome => status(
            if outcome == CONFLICT {
                "conflict"
            } else {
                "blocked"
            },
            "The change was rolled back. Close XSOverlay and check Steam library write access.",
            false,
        ),
    }
}

fn steam_path() -> PathBuf {
    let default = env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:/Program Files (x86)"))
        .join("Steam");
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Valve\Steam")
        .and_then(|key| key.get_value::<String, _>("SteamPath"))
        .map(PathBuf::from)
        .ok()
        .filter(|path| path.is_absolute())
        .unwrap_or(default)
}

fn library_paths(steam: &Path) -> Vec<PathBuf> {
    static PATHS: OnceLock<regex::Regex> = OnceLock::new();
    let mut libraries = vec![steam.to_path_buf()];
    if let Ok(Some(vdf)) = read(&steam.join("steamapps/libraryfolders.vdf")) {
        let text = String::from_utf8_lossy(&vdf);
        let paths = PATHS
            .get_or_init(|| regex::Regex::new(r#"(?i)"(?:path|[0-9]+)"\s*"([^"]+)""#).unwrap());
        libraries.extend(paths.captures_iter(&text).filter_map(|capture| {
            let path = PathBuf::from(capture.get(1)?.as_str().replace("\\\\", "\\"));
            path.is_absolute().then_some(path)
        }));
    }
    libraries
}

fn discover_in(steam: &Path) -> Option<PathBuf> {
    let mut seen = HashSet::new();
    let mut first = None;
    for library in library_paths(steam) {
        if !guard_path(&library, true).unwrap_or(false) {
            continue;
        }
        let Ok(canonical) = library.canonicalize() else {
            continue;
        };
        if !seen.insert(canonical.to_string_lossy().to_lowercase()) {
            continue;
        }
        for product in ["XSOverlay_Beta", "XSOverlay"] {
            let root = canonical
                .join("steamapps/common")
                .join(product)
                .join("XSOverlay_Data/StreamingAssets/Plugins/Applications");
            if !guard_path(&root.join("_UI/Default/Wrist.html"), false).unwrap_or(false) {
                continue;
            }
            let proof = read(&root.join(NAME).join(MANAGED[0]))
                .ok()
                .flatten()
                .and_then(|data| serde_json::from_slice::<Value>(&data).ok());
            if proof.is_some_and(|proof| {
                ownership(&json!({"receipt":proof,"hasReceipt":true,"directory":true}))
                    == Ownership::Installed
            }) {
                return Some(root);
            }
            first.get_or_insert(root);
        }
    }
    first
}

pub fn discover() -> Option<PathBuf> {
    discover_in(&steam_path())
}

fn copy_output(value: &Value, output: *mut u8, capacity: usize) -> usize {
    let Ok(body) = serde_json::to_vec(value) else {
        return 0;
    };
    if output.is_null() || body.len() > capacity {
        return 0;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(body.as_ptr(), output, body.len());
    }
    body.len()
}

#[no_mangle]
pub extern "C" fn lummix_xsoverlay_discover(output: *mut u8, capacity: usize) -> usize {
    let path = discover();
    copy_output(&json!(path), output, capacity)
}

#[no_mangle]
pub unsafe extern "C" fn lummix_xsoverlay_addon(
    input: *const u8,
    length: usize,
    action: u32,
    output: *mut u8,
    capacity: usize,
) -> usize {
    if input.is_null() || length == 0 || length > 384_000 || action > 2 {
        return 0;
    }
    let Ok(paths) = serde_json::from_slice::<Value>(slice::from_raw_parts(input, length)) else {
        return 0;
    };
    let Some(data) = paths["data"].as_str() else {
        return 0;
    };
    let root = paths["root"].as_str().map(Path::new);
    copy_output(&addon(root, Path::new(data), action), output, capacity)
}

#[cfg(test)]
mod native_tests {
    use super::*;

    struct TempTree(PathBuf);
    impl TempTree {
        fn new() -> Self {
            let root = env::temp_dir().join(format!(
                "lummix-addon-{}",
                crate::private_http::token().unwrap()
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn vendor(&self, library: &Path, product: &str) -> PathBuf {
            let root = library
                .join("steamapps/common")
                .join(product)
                .join("XSOverlay_Data/StreamingAssets/Plugins/Applications");
            fs::create_dir_all(root.join("_UI/Default")).unwrap();
            fs::write(
                root.join("_UI/Default/Wrist.html"),
                b"<body id=uiContainer>./_Shared/js/uiComponents.js ./_Shared/js/toolbar.js</body>",
            )
            .unwrap();
            root
        }
        fn bundled(&self) -> PathBuf {
            let data = self.0.join("data");
            fs::create_dir(&data).unwrap();
            fs::write(data.join("xsoverlay-live.js"), b"/* fixture */").unwrap();
            fs::write(data.join("xsoverlay-icon.png"), b"\x89PNG\r\n\x1a\nfixture").unwrap();
            data
        }
    }
    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn native_rollback_keeps_concurrently_modified_icon_bytes() {
        let tree = TempTree::new();
        let root = tree.vendor(&tree.0, "XSOverlay");
        let data = tree.bundled();
        let state = inspected(&root, &data);
        assert_eq!(state.result["state"], "installable");
        let wrist = root.join("_UI/Default/Wrist.html");
        let stock = fs::read(&wrist).unwrap();
        let _protected = OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&wrist)
            .unwrap();
        let outcome = apply_snapshot(state.snapshot.unwrap(), true, |phase, slot| {
            if phase == Phase::Write && slot == 3 {
                fs::write(root.join("Lummix/icon.png"), b"user replacement").unwrap();
            }
        });
        assert_eq!(outcome, ROLLBACK_INCOMPLETE);
        assert_eq!(fs::read(wrist).unwrap(), stock);
        assert_eq!(
            fs::read(root.join("Lummix/icon.png")).unwrap(),
            b"user replacement"
        );
        assert!(!root.join("Lummix/app.json").exists());
    }

    #[test]
    fn library_discovery_deduplicates_paths_and_prefers_a_prior_owned_installation() {
        let tree = TempTree::new();
        let other = TempTree::new();
        let beta = tree.vendor(&tree.0, "XSOverlay_Beta");
        let stable = other.vendor(&other.0, "XSOverlay");
        let path = other.0.to_string_lossy().replace('\\', "\\\\");
        fs::write(
            tree.0.join("steamapps/libraryfolders.vdf"),
            format!(
                r#"
"libraryfolders" {{ "0" {{ "path" "{path}" }} "1" "{path}" "2" {{ "path" "{path}" }} }}
"#
            ),
        )
        .unwrap();
        assert_eq!(discover_in(&tree.0), Some(beta.canonicalize().unwrap()));
        let data = tree.bundled();
        assert_eq!(addon(Some(&stable), &data, 1)["state"], "ready");
        assert_eq!(discover_in(&tree.0), Some(stable.canonicalize().unwrap()));
    }

    #[test]
    fn present_but_read_only_install_is_ready_without_remove_permission() {
        assert_eq!(
            disposition(true, false, Ownership::Current as u32),
            Disposition::ReadyReadOnly as u32
        );
        assert_eq!(
            disposition(true, false, Ownership::Uninstalled as u32),
            Disposition::Blocked as u32
        );
    }
}
