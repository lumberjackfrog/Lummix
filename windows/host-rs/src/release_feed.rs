use std::ffi::{c_void, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use windows_sys::core::w;
use windows_sys::Win32::Networking::WinHttp::*;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};

use crate::local_http::LoopbackClient;
use crate::update::{verify, Sha256};

const SOURCE: &str = include_str!("../../release_identity.py");
const REPO: &str = "https://github.com/lumberjackfrog/Lummix/releases";
const API: &str = "https://api.github.com/repos/lumberjackfrog/Lummix/releases";
const ASSET: &str = "Lummix.Setup.exe";
const MANIFEST: &str = "Lummix.Update.json";
const PART: &str = "download.part";
const LIMIT: u64 = 128 * 1024 * 1024;
const MANIFEST_LIMIT: usize = 8192;
const CHUNK: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Invalid = 1,
    Network = 2,
    Storage = 3,
    Cancelled = 4,
    Current = 5,
}

pub(crate) type Outcome<T> = Result<T, Failure>;

pub(crate) fn identity(name: &str) -> Outcome<&'static str> {
    SOURCE
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(" = ")?;
            (key == name).then(|| value.trim_matches('"'))
        })
        .ok_or(Failure::Invalid)
}

fn version(value: &str) -> Option<[u16; 3]> {
    let mut components = value.split('.');
    let mut result = [0; 3];
    for target in &mut result {
        let field = components.next()?;
        if field.is_empty()
            || field.len() > 4
            || (field.len() > 1 && field.starts_with('0'))
            || !field.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        *target = field.parse().ok()?;
    }
    components.next().is_none().then_some(result)
}

fn tag_version(tag: &str) -> Option<[u16; 3]> {
    version(tag.strip_prefix('v')?)
}

fn split_url(url: &str) -> Outcome<(&str, &str)> {
    if !url
        .bytes()
        .all(|byte| (0x21..=0x7e).contains(&byte) && !matches!(byte, b'#' | b'\\'))
    {
        return Err(Failure::Invalid);
    }
    let rest = url.strip_prefix("https://").ok_or(Failure::Invalid)?;
    let slash = rest.find('/').ok_or(Failure::Invalid)?;
    let (host, path) = rest.split_at(slash);
    if !matches!(
        host,
        "api.github.com"
            | "github.com"
            | "release-assets.githubusercontent.com"
            | "objects.githubusercontent.com"
            | "github-releases.githubusercontent.com"
    ) {
        return Err(Failure::Invalid);
    }
    Ok((host, path))
}

fn redirect(host: &str, location: &str) -> Outcome<String> {
    let url = if location.starts_with("//") {
        format!("https:{location}")
    } else if location.starts_with('/') {
        format!("https://{host}{location}")
    } else {
        location.to_owned()
    };
    split_url(&url)?;
    Ok(url)
}

struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void) -> Outcome<Self> {
        if raw.is_null() {
            return Err(Failure::Network);
        }
        Ok(Self(raw))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn network(success: i32) -> Outcome<()> {
    if success == 0 {
        Err(Failure::Network)
    } else {
        Ok(())
    }
}

fn text_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

struct Internet(Handle);
struct Response {
    request: Handle,
    _connection: Handle,
}

impl Internet {
    fn new() -> Outcome<Self> {
        let session = Handle::new(unsafe {
            WinHttpOpen(
                w!("Lummix-Windows-Updater"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                null(),
                null(),
                0,
            )
        })?;
        network(unsafe { WinHttpSetTimeouts(session.0, 15_000, 15_000, 15_000, 15_000) })?;
        Ok(Self(session))
    }

    fn get(&self, host: &str, path: &str) -> Outcome<(u32, Response)> {
        let (host_wide, path_wide) = (text_wide(host), text_wide(path));
        let connection = Handle::new(unsafe {
            WinHttpConnect(
                self.0 .0,
                host_wide.as_ptr(),
                INTERNET_DEFAULT_HTTPS_PORT,
                0,
            )
        })?;
        let request = Handle::new(unsafe {
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                path_wide.as_ptr(),
                null(),
                null(),
                null(),
                WINHTTP_FLAG_SECURE | WINHTTP_FLAG_ESCAPE_DISABLE,
            )
        })?;
        let disabled =
            WINHTTP_DISABLE_REDIRECTS | WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_AUTHENTICATION;
        network(unsafe {
            WinHttpSetOption(
                request.0,
                WINHTTP_OPTION_DISABLE_FEATURE,
                (&disabled as *const u32).cast(),
                size_of::<u32>() as u32,
            )
        })?;
        network(unsafe {
            WinHttpSendRequest(
                request.0,
                w!("Accept-Encoding: identity\r\n"),
                u32::MAX,
                null(),
                0,
                0,
                0,
            )
        })?;
        network(unsafe { WinHttpReceiveResponse(request.0, null_mut()) })?;
        let mut status = 0u32;
        let mut length = size_of::<u32>() as u32;
        network(unsafe {
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&mut status as *mut u32).cast(),
                &mut length,
                null_mut(),
            )
        })?;
        Ok((
            status,
            Response {
                request,
                _connection: connection,
            },
        ))
    }

    fn open(&self, url: &str, deadline: Instant) -> Outcome<Response> {
        let mut current = url.to_owned();
        for _ in 0..=10 {
            if Instant::now() >= deadline {
                return Err(Failure::Network);
            }
            let (host, path) = split_url(&current)?;
            let (status, response) = self.get(host, path)?;
            if status == 200 {
                return Ok(response);
            }
            if !matches!(status, 301 | 302 | 303 | 307 | 308) {
                return Err(Failure::Network);
            }
            let mut location = [0u16; 8192];
            let mut length = size_of_val(&location) as u32;
            let success = unsafe {
                WinHttpQueryHeaders(
                    response.request.0,
                    WINHTTP_QUERY_LOCATION,
                    null(),
                    location.as_mut_ptr().cast(),
                    &mut length,
                    null_mut(),
                )
            };
            if success == 0 {
                return Err(Failure::Invalid);
            }
            let location = String::from_utf16(&location[..length as usize / 2])
                .map_err(|_| Failure::Invalid)?;
            current = redirect(host, &location)?;
        }
        Err(Failure::Invalid)
    }

    fn json(&self, url: &str, limit: usize) -> Outcome<Value> {
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut response = self.open(url, deadline)?;
        let mut bytes = Vec::new();
        let mut block = [0u8; 8192];
        loop {
            let maximum = block.len().min(limit + 1 - bytes.len());
            let count = response
                .read(&mut block[..maximum])
                .map_err(|_| Failure::Network)?;
            if Instant::now() >= deadline {
                return Err(Failure::Network);
            }
            bytes.extend_from_slice(&block[..count]);
            if bytes.len() > limit {
                return Err(Failure::Invalid);
            }
            if count == 0 {
                break;
            }
        }
        serde_json::from_slice(&bytes).map_err(|_| Failure::Invalid)
    }
}

impl Read for Response {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut count = 0;
        let success = unsafe {
            WinHttpReadData(
                self.request.0,
                buf.as_mut_ptr().cast(),
                buf.len().min(u32::MAX as usize) as u32,
                &mut count,
            )
        };
        if success == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(count as usize)
    }
}

/// Fetch the reviewed VB-Audio ZIP directly. Do not follow CDN or cross-host redirects.
pub(crate) fn vendor_archive() -> Outcome<Vec<u8>> {
    let internet = Internet::new()?;
    let (status, mut response) = internet.get(
        "download.vb-audio.com",
        "/Download_CABLE/VBCABLE_Driver_Pack46.zip",
    )?;
    if status != 200 {
        return Err(Failure::Network);
    }
    let limit = crate::cable_setup::ARCHIVE_SIZE;
    let deadline = Instant::now() + Duration::from_secs(55);
    let mut bytes = Vec::with_capacity(limit);
    let mut block = [0; 16384];
    loop {
        let n = response.read(&mut block).map_err(|_| Failure::Network)?;
        if Instant::now() >= deadline || n > limit - bytes.len() {
            return Err(Failure::Invalid);
        }
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&block[..n]);
    }
    if bytes.len() != limit {
        return Err(Failure::Invalid);
    }
    Ok(bytes)
}

struct Release<'a> {
    version: &'a str,
    size: u64,
    digest: [u8; 32],
}

impl Release<'_> {
    fn url(&self) -> String {
        format!("{REPO}/download/v{}/{ASSET}", self.version)
    }
}

fn sha_from_hex(digest: &str) -> Outcome<[u8; 32]> {
    let mut result = [0; 32];
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Failure::Invalid);
    }
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digest[index * 2..index * 2 + 2], 16)
            .map_err(|_| Failure::Invalid)?;
    }
    Ok(result)
}

fn signed(document: &Value) -> Outcome<Release<'_>> {
    let tag = document["tag"]
        .as_str()
        .filter(|tag| tag_version(tag).is_some())
        .ok_or(Failure::Invalid)?;
    let size = document["size"]
        .as_u64()
        .filter(|size| (1..=LIMIT).contains(size))
        .ok_or(Failure::Invalid)?;
    let hex = document["sha256"].as_str().ok_or(Failure::Invalid)?;
    let digest = sha_from_hex(hex)?;
    if document["format"].as_u64() != Some(1) {
        return Err(Failure::Invalid);
    }
    let signature = STANDARD
        .decode(document["signature"].as_str().ok_or(Failure::Invalid)?)
        .map_err(|_| Failure::Invalid)?;
    let key = STANDARD
        .decode(identity("PUBLIC_KEY")?)
        .map_err(|_| Failure::Invalid)?;
    let canonical = format!("Lummix Windows x64 update v1\n{tag}\n{size}\n{hex}\n");
    verify(canonical.as_bytes(), &signature, &key).map_err(|_| Failure::Invalid)?;
    Ok(Release {
        version: &tag[1..],
        size,
        digest,
    })
}

fn published(row: &Value) -> bool {
    row["draft"].as_bool() == Some(false)
        && row["published_at"]
            .as_str()
            .is_some_and(|stamp| !stamp.is_empty())
}

fn select<'a>(rows: &'a Value, current: &str) -> Outcome<&'a Value> {
    let mut best = version(current).ok_or(Failure::Invalid)?;
    let mut selected = None;
    for row in rows.as_array().ok_or(Failure::Invalid)? {
        if !row.is_object() {
            return Err(Failure::Invalid);
        }
        if let Some(number) = row["tag_name"].as_str().and_then(tag_version) {
            if number > best && published(row) {
                selected = Some(row);
                best = number;
            }
        }
    }
    selected.ok_or(Failure::Current)
}

fn qualify(row: &Value, fetch: impl FnOnce(&str) -> Outcome<Value>) -> Outcome<Value> {
    let tag = row["tag_name"]
        .as_str()
        .filter(|tag| tag_version(tag).is_some())
        .ok_or(Failure::Invalid)?;
    if !published(row) || row["html_url"] != format!("{REPO}/tag/{tag}") {
        return Err(Failure::Invalid);
    }
    let mut assets = [None, None];
    for asset in row["assets"].as_array().ok_or(Failure::Invalid)? {
        if !asset.is_object() {
            return Err(Failure::Invalid);
        }
        let which = match asset["name"].as_str() {
            Some(ASSET) => 0,
            Some(MANIFEST) => 1,
            _ => continue,
        };
        if asset["state"] == "uploaded" {
            let name = [ASSET, MANIFEST][which];
            if assets[which].replace(asset).is_some()
                || asset["browser_download_url"] != format!("{REPO}/download/{tag}/{name}")
            {
                return Err(Failure::Invalid);
            }
        }
    }
    let [Some(exe), Some(manifest)] = assets else {
        return Err(Failure::Invalid);
    };
    if !manifest["size"]
        .as_u64()
        .is_some_and(|size| (1..=MANIFEST_LIMIT as u64).contains(&size))
    {
        return Err(Failure::Invalid);
    }
    let expected = exe["digest"]
        .as_str()
        .and_then(|value| value.strip_prefix("sha256:"))
        .ok_or(Failure::Invalid)?;
    if exe["size"].as_u64().is_none() {
        return Err(Failure::Invalid);
    }
    let document = fetch(&format!("{REPO}/download/{tag}/{MANIFEST}"))?;
    if document["tag"] != tag || document["size"] != exe["size"] || document["sha256"] != expected {
        return Err(Failure::Invalid);
    }
    signed(&document)?;
    Ok(normalized(&document))
}

fn normalized(document: &Value) -> Value {
    json!({
        "format": document["format"], "tag": document["tag"], "size": document["size"],
        "sha256": document["sha256"], "signature": document["signature"],
    })
}

fn plain(path: &Path) -> Outcome<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
            Err(Failure::Invalid)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Failure::Storage),
    }
}

fn unlink(path: &Path) -> Outcome<()> {
    plain(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(Failure::Storage),
    }
}

pub(crate) fn cleanup(directory: &Path) -> Outcome<()> {
    plain(directory)?;
    if !directory.exists() {
        return Ok(());
    }
    let _directory = lock_path(directory, true)?;
    cleanup_files(directory)
}

fn cleanup_files(directory: &Path) -> Outcome<()> {
    unlink(&directory.join(ASSET))?;
    unlink(&directory.join(PART))
}

struct Partial(PathBuf, bool);

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.1 {
            let _ = fs::remove_file(&self.0);
        }
    }
}

fn bounded_hash(
    mut input: impl Read,
    mut output: impl Write,
    release: &Release<'_>,
    allowed: impl Fn() -> bool,
    read_failure: Failure,
    deadline: Instant,
) -> Outcome<()> {
    let mut hasher = Sha256::open().map_err(|_| Failure::Storage)?;
    let mut buf = vec![0; CHUNK];
    let mut total = 0u64;
    loop {
        if !allowed() {
            return Err(Failure::Cancelled);
        }
        let remaining = release.size + 1 - total;
        let limit = buf.len().min(remaining as usize);
        let size = input.read(&mut buf[..limit]).map_err(|_| read_failure)?;
        total += size as u64;
        if total > release.size || Instant::now() >= deadline {
            return Err(Failure::Invalid);
        }
        if size == 0 {
            break;
        }
        hasher.update(&buf[..size]).map_err(|_| Failure::Storage)?;
        output
            .write_all(&buf[..size])
            .map_err(|_| Failure::Storage)?;
    }
    if !allowed() {
        return Err(Failure::Cancelled);
    }
    if total != release.size || hasher.finish().map_err(|_| Failure::Storage)? != release.digest {
        return Err(Failure::Invalid);
    }
    Ok(())
}

fn stage<R: Read>(
    directory: &Path,
    release: &Release<'_>,
    open: impl FnOnce(Instant) -> Outcome<R>,
    allowed: impl Fn() -> bool,
) -> Outcome<()> {
    plain(directory)?;
    fs::create_dir_all(directory).map_err(|_| Failure::Storage)?;
    let _directory = lock_path(directory, true)?;
    cleanup_files(directory)?;
    if !allowed() {
        return Err(Failure::Cancelled);
    }
    let deadline = Instant::now() + Duration::from_secs(600);
    let response = open(deadline)?;
    let path = directory.join(PART);
    let mut partial = Partial(path, true);
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial.0)
        .map_err(|_| Failure::Storage)?;
    partial.1 = false;
    bounded_hash(
        response,
        &mut target,
        release,
        &allowed,
        Failure::Network,
        deadline,
    )?;
    target.sync_all().map_err(|_| Failure::Storage)?;
    drop(target);
    let mut ads = partial.0.as_os_str().to_os_string();
    ads.push(":Zone.Identifier");
    fs::write(
        ads,
        format!(
            "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={}\r\n",
            release.url()
        ),
    )
    .map_err(|_| Failure::Storage)?;
    if !allowed() {
        return Err(Failure::Cancelled);
    }
    fs::rename(&partial.0, directory.join(ASSET)).map_err(|_| Failure::Storage)?;
    partial.1 = true;
    Ok(())
}

fn lock_path(path: &Path, directory: bool) -> Outcome<File> {
    let share = FILE_SHARE_READ | if directory { FILE_SHARE_WRITE } else { 0 };
    let options = FILE_FLAG_OPEN_REPARSE_POINT
        | if directory {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        };
    let file = OpenOptions::new()
        .read(true)
        .share_mode(share)
        .custom_flags(options)
        .open(path)
        .map_err(|_| Failure::Storage)?;
    if file
        .metadata()
        .map_err(|_| Failure::Storage)?
        .file_attributes()
        & FILE_ATTRIBUTE_REPARSE_POINT
        != 0
    {
        return Err(Failure::Invalid);
    }
    Ok(file)
}

fn install(
    directory: &Path,
    release: &Release<'_>,
    allowed: impl Fn() -> bool,
    open_shell: impl FnOnce(&Path) -> Outcome<()>,
) -> Outcome<()> {
    if !allowed() {
        return Err(Failure::Cancelled);
    }
    let _directory = lock_path(directory, true)?;
    let path = directory.join(ASSET);
    let file = lock_path(&path, false)?;
    if file.metadata().map_err(|_| Failure::Storage)?.len() != release.size {
        return Err(Failure::Invalid);
    }
    bounded_hash(
        &file,
        io::sink(),
        release,
        || true,
        Failure::Storage,
        Instant::now() + Duration::from_secs(600),
    )?;
    if !allowed() {
        return Err(Failure::Cancelled);
    }
    open_shell(&path)
}

fn shell(path: &Path) -> Outcome<()> {
    use windows_sys::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    let path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let parameters = text_wide(&format!("/LUMMIXWAITPID={}", std::process::id()));
    let apartment = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            w!("open"),
            path.as_ptr(),
            parameters.as_ptr(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    if apartment >= 0 {
        unsafe { CoUninitialize() };
    }
    if result as isize <= 32 {
        return Err(Failure::Storage);
    }
    Ok(())
}

fn route_idle() -> bool {
    let Ok(http) = LoopbackClient::new() else {
        return false;
    };
    let Ok(reply) = http.get(41838, "/mix/gains", true, 8193) else {
        return false;
    };
    idle_response(reply.status, reply.studio_cors, &reply.body)
}

fn idle_response(status: u32, cors: bool, body: &[u8]) -> bool {
    status == 200
        && cors
        && body.len() <= 8192
        && serde_json::from_slice::<Value>(body)
            .is_ok_and(|value| value["routeActive"].as_bool() == Some(false))
}

pub(crate) fn latest_version(current: &str) -> Outcome<Option<Value>> {
    let internet = Internet::new()?;
    let releases = internet.json(&format!("{API}?per_page=30"), 1024 * 1024)?;
    match select(&releases, current) {
        Ok(row) => qualify(row, |origin| internet.json(origin, MANIFEST_LIMIT)).map(Some),
        Err(Failure::Current) => Ok(None),
        Err(error) => Err(error),
    }
}

fn higher_release(document: &Value) -> Outcome<Release<'_>> {
    let release = signed(document)?;
    let running = version(identity("VERSION")?).ok_or(Failure::Invalid)?;
    if version(release.version).is_some_and(|number| number > running) {
        Ok(release)
    } else {
        Err(Failure::Invalid)
    }
}

pub(crate) fn download_release(
    directory: &Path,
    document: &Value,
    allowed: impl Fn() -> bool,
) -> Outcome<()> {
    let release = higher_release(document)?;
    let internet = Internet::new()?;
    stage(
        directory,
        &release,
        |deadline| internet.open(&release.url(), deadline),
        allowed,
    )
}

pub(crate) fn install_release(
    directory: &Path,
    document: &Value,
    allowed: impl Fn() -> bool,
) -> Outcome<()> {
    let release = higher_release(document)?;
    install(directory, &release, || allowed() && route_idle(), shell)
}

pub(crate) fn checked_recently(directory: &Path, release: &str) -> bool {
    let stamp = directory.join("last-check");
    let age = || -> Option<bool> {
        plain(directory).ok()?;
        plain(&stamp).ok()?;
        let metadata = fs::metadata(&stamp).ok()?;
        let now = std::time::SystemTime::now();
        let modified = metadata.modified().ok()?;
        if metadata.len() > 20
            || !(now
                .duration_since(modified)
                .is_ok_and(|age| age < Duration::from_secs(21_600))
                || modified
                    .duration_since(now)
                    .is_ok_and(|advance| advance <= Duration::from_secs(1)))
        {
            return Some(false);
        }
        Some(fs::read(stamp).ok()? == release.as_bytes())
    };
    age().unwrap_or(false)
}

pub(crate) fn write_check(directory: &Path, release: &str) -> Outcome<()> {
    plain(directory)?;
    fs::create_dir_all(directory).map_err(|_| Failure::Storage)?;
    let _lock = lock_path(directory, true)?;
    let stamp = directory.join("last-check");
    unlink(&stamp)?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stamp)
        .and_then(|mut file| file.write_all(release.as_bytes()))
        .map_err(|_| Failure::Storage)
}

type Output = extern "C" fn(*const u8, usize);
type Continue = extern "C" fn() -> i32;

fn perform(
    action: u32,
    input: &[u8],
    path: &Path,
    output: Option<Output>,
    gate: Option<Continue>,
) -> Outcome<()> {
    if matches!(action, 2..=4) && !path.is_absolute() {
        return Err(Failure::Invalid);
    }
    match action {
        0 | 1 => {
            let input = std::str::from_utf8(input).map_err(|_| Failure::Invalid)?;
            let internet = Internet::new()?;
            let row = if action == 0 {
                let rows = internet.json(&format!("{API}?per_page=30"), 1024 * 1024)?;
                select(&rows, input)?.clone()
            } else {
                tag_version(input).ok_or(Failure::Invalid)?;
                let row = internet.json(&format!("{API}/tags/{input}"), 256 * 1024)?;
                if row["tag_name"] != input {
                    return Err(Failure::Invalid);
                }
                row
            };
            let document = qualify(&row, |url| internet.json(url, MANIFEST_LIMIT))?;
            deliver(output, &document)
        }
        2 | 3 => {
            if input.len() > MANIFEST_LIMIT {
                return Err(Failure::Invalid);
            }
            let document = serde_json::from_slice(input).map_err(|_| Failure::Invalid)?;
            let callback = gate.ok_or(Failure::Invalid)?;
            let allowed = || callback() != 0;
            if action == 2 {
                download_release(path, &document, allowed)
            } else {
                install_release(path, &document, allowed)
            }
        }
        4 => cleanup(path),
        5 => {
            let document = serde_json::from_slice(input).map_err(|_| Failure::Invalid)?;
            signed(&document)?;
            deliver(output, &normalized(&document))
        }
        _ => Err(Failure::Invalid),
    }
}

fn deliver(output: Option<Output>, document: &Value) -> Outcome<()> {
    let data = serde_json::to_vec(document).map_err(|_| Failure::Invalid)?;
    let output = output.ok_or(Failure::Invalid)?;
    output(data.as_ptr(), data.len());
    Ok(())
}

/// The input spans and callbacks belong to the caller for the duration of the synchronous operation.
#[no_mangle]
pub unsafe extern "C" fn lummix_update(
    action: u32,
    input: *const u8,
    size: usize,
    path: *const u16,
    path_len: usize,
    output: Option<Output>,
    gate: Option<Continue>,
) -> u32 {
    if size > MANIFEST_LIMIT || (input.is_null() && size != 0) || (path.is_null() && path_len != 0)
    {
        return Failure::Invalid as u32;
    }
    let input = if size == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(input, size)
    };
    let path = if path_len == 0 {
        PathBuf::new()
    } else {
        PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(
            path, path_len,
        )))
    };
    perform(action, input, &path, output, gate).map_or_else(|error| error as u32, |()| 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn historical() -> Value {
        json!({"format":1,"tag":"v1.0.7","size":42737340,
            "sha256":"d25f4c5000e99ab5a440f3db8bc957adb24b3a7b8aee68991fd2e6ab3bf0c907",
            "signature":"Owlkc2kTdwAmmij6niAa4O59y8smF109wS+WzQ1Vo34Mzt5Lt/w0GvufpnDtwjrywhIgFHnAV2ak9Zz37e7kdg=="})
    }

    fn release_row(document: &Value) -> Value {
        let tag = document["tag"].as_str().unwrap();
        json!({"tag_name":tag,"draft":false,"published_at":"2026-11-26T00:00:00Z",
        "html_url":format!("{REPO}/tag/{tag}"),
        "assets":[
            {"name":ASSET,"state":"uploaded","size":document["size"],
                "digest":format!("sha256:{}",document["sha256"].as_str().unwrap()),
                "browser_download_url":format!("{REPO}/download/{tag}/{ASSET}")},
            {"name":MANIFEST,"state":"uploaded","size":500,
                "browser_download_url":format!("{REPO}/download/{tag}/{MANIFEST}")}
        ]})
    }

    #[test]
    fn verifies_official_identity_selects_only_newer_versions_and_rejects_tampering() {
        let document = historical();
        let row = release_row(&document);
        let mut drafts = row.clone();
        drafts["draft"] = true.into();
        drafts["tag_name"] = "v1.0.8".into();
        let rows = json!([drafts, row]);
        let selected = select(&rows, "1.0.6").unwrap();
        assert_eq!(
            qualify(selected, |_| Ok(document.clone())).unwrap(),
            document
        );
        assert_eq!(select(&rows, "1.0.7"), Err(Failure::Current));
        assert_eq!(select(&rows, "1.0.8"), Err(Failure::Current));
        assert_eq!(select(&rows, "01.0.8"), Err(Failure::Invalid));
        assert_eq!(
            perform(
                2,
                &serde_json::to_vec(&document).unwrap(),
                Path::new(r"C:\"),
                None,
                None
            ),
            Err(Failure::Invalid)
        );
        for (field, value) in [
            ("format", json!(true)),
            ("sha256", json!("0".repeat(64))),
            ("signature", json!(STANDARD.encode([0; 64]))),
            ("size", json!(123)),
        ] {
            let mut changed = document.clone();
            changed[field] = value;
            assert!(matches!(signed(&changed), Err(Failure::Invalid)));
        }
        let mut changed = row.clone();
        changed["assets"][0]["browser_download_url"] = "https://example.org/setup.exe".into();
        assert_eq!(qualify(&changed, |_| unreachable!()), Err(Failure::Invalid));
        let duplicate = changed["assets"][1].clone();
        changed["assets"].as_array_mut().unwrap().push(duplicate);
        assert_eq!(qualify(&changed, |_| unreachable!()), Err(Failure::Invalid));
    }

    #[test]
    fn every_redirect_stays_https_and_within_the_publishers_hosts() {
        for url in [
            "http://github.com/a",
            "https://github.com.evil.invalid/a",
            "https://alice@github.com/a",
            "https://github.com:443/a",
            "file:///C:/a",
            "https://github.com/a#b",
            "https://github.com\\@evil.invalid/a",
            "https://github.com/a\r\nInjected: Content",
        ] {
            assert_eq!(split_url(url), Err(Failure::Invalid), "{url}");
        }
        assert_eq!(
            redirect("github.com", "/next").unwrap(),
            "https://github.com/next"
        );
        assert!(redirect(
            "github.com",
            "//release-assets.githubusercontent.com/path?test=a%2Fb"
        )
        .is_ok());
        assert_eq!(
            redirect("github.com", "//untrusted.invalid/path"),
            Err(Failure::Invalid)
        );
    }

    #[test]
    fn install_requires_a_bounded_explicit_idle_reply_from_the_media_origin() {
        let valid = br#"{"routeActive":false}"#;
        assert!(idle_response(200, true, valid));
        assert!(!idle_response(200, false, valid));
        assert!(!idle_response(503, true, valid));
        assert!(!idle_response(200, true, br#"{"routeActive":0}"#));
        assert!(!idle_response(200, true, br#"{"routeActive":true}"#));
        let mut padded = valid.to_vec();
        padded.resize(8193, b' ');
        assert!(!idle_response(200, true, &padded));
    }

    #[test]
    #[ignore = "requires outbound access to GitHub; requests release metadata only"]
    fn actual_public_release_metadata_round_trip() {
        let tag = "v1.0.8";
        let internet = Internet::new().unwrap();
        let row = internet
            .json(&format!("{API}/tags/{tag}"), 256 * 1024)
            .unwrap();
        let manifest = qualify(&row, |url| internet.json(url, MANIFEST_LIMIT)).unwrap();
        assert_eq!(manifest["tag"], tag);
        assert_eq!(
            manifest["sha256"],
            "31237da77e20e5d5ea21c5aecbe86d36f05cf8ddb28d022139ff50e2f4d4afef"
        );
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("LummixUpdaterFixture-{}", std::process::id()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn stages_exact_bytes_with_motw_and_rehashes_under_lock_after_explicit_gate() {
        let fixture = Fixture::new();
        let directory = &fixture.0;
        let bytes = vec![0x42; 2 * CHUNK + 5];
        let mut sha = Sha256::open().unwrap();
        sha.update(&bytes).unwrap();
        let release = Release {
            version: "9.0.0",
            size: bytes.len() as u64,
            digest: sha.finish().unwrap(),
        };
        let wrong = [0xaa];
        assert_eq!(
            stage(directory, &release, |_| Ok(wrong.as_slice()), || true),
            Err(Failure::Invalid)
        );
        assert_eq!(directory.read_dir().unwrap().count(), 0);
        let mut too_long = bytes.clone();
        too_long.push(0x42);
        assert_eq!(
            stage(directory, &release, |_| Ok(too_long.as_slice()), || true),
            Err(Failure::Invalid)
        );
        assert_eq!(directory.read_dir().unwrap().count(), 0);
        let checks = Cell::new(0);
        assert_eq!(
            stage(
                directory,
                &release,
                |_| Ok(bytes.as_slice()),
                || {
                    let checks_before = checks.replace(checks.get() + 1);
                    checks_before < 2
                }
            ),
            Err(Failure::Cancelled)
        );
        assert_eq!(directory.read_dir().unwrap().count(), 0);
        stage(directory, &release, |_| Ok(bytes.as_slice()), || true).unwrap();
        let setup = directory.join(ASSET);
        assert_eq!(fs::read(&setup).unwrap(), bytes);
        let mut ads = setup.clone().into_os_string();
        ads.push(":Zone.Identifier");
        assert!(fs::read_to_string(ads).unwrap().contains("ZoneId=3"));
        assert_eq!(
            install(directory, &release, || false, |_| unreachable!()),
            Err(Failure::Cancelled)
        );
        let first = Cell::new(true);
        assert_eq!(
            install(
                directory,
                &release,
                || first.replace(false),
                |_| unreachable!()
            ),
            Err(Failure::Cancelled)
        );
        install(
            directory,
            &release,
            || true,
            |path| {
                assert_eq!(path, setup);
                assert!(fs::write(path, b"modified").is_err());
                assert!(fs::remove_file(path).is_err());
                Ok(())
            },
        )
        .unwrap();
        let mut forged = bytes.clone();
        forged[0] ^= 1;
        fs::write(&setup, forged).unwrap();
        assert_eq!(
            install(directory, &release, || true, |_| unreachable!()),
            Err(Failure::Invalid)
        );
        cleanup(directory).unwrap();
        assert_eq!(directory.read_dir().unwrap().count(), 0);
    }
}
