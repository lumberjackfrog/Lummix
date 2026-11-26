use super::{processes, source, store_paths};
use serde_json::{json, Value};
use std::{
    ffi::{c_void, OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    ptr::{null, null_mut},
    thread::sleep,
    time::{Duration, Instant},
};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::HWND,
        Networking::WinHttp::*,
        System::SystemInformation::GetSystemDirectoryW,
        UI::{
            Shell::{IsUserAnAdmin, ShellExecuteW},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
};

const URL: &str = "https://download.scdn.co/SpotifySetup.exe";
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const CHECK_SIGNATURE: &str = r"
$ErrorActionPreference = 'Stop'
$s = Get-AuthenticodeSignature -LiteralPath $env:LUMMIX_SPOTIFY_FILE
if ($s.Status -ne 'Valid' -or $null -eq $s.SignerCertificate -or
    $s.SignerCertificate.GetNameInfo('SimpleName', $false) -cne 'Spotify AB') { exit 1 }
";

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Lummix closed")
}

#[derive(Debug)]
enum Installation {
    Desktop(PathBuf),
    Store,
}

trait Setup {
    fn running(&self) -> io::Result<bool>;
    fn installed(&self) -> io::Result<Option<Installation>>;
    fn elevated(&self) -> bool;
    fn installer_running(&self) -> io::Result<bool>;
    fn download(&self, notify: &mut dyn FnMut(u32) -> bool) -> io::Result<()>;
    fn launch(
        &self,
        choice: Option<Installation>,
        notify: &mut dyn FnMut(u32) -> bool,
    ) -> io::Result<()>;
    fn wait(&self, notify: &mut dyn FnMut(u32) -> bool) -> bool;
}

fn prepare(setup: &impl Setup, notify: &mut dyn FnMut(u32) -> bool) -> io::Result<&'static str> {
    if setup.running()? {
        return Ok("ready");
    }
    let installed = setup.installed()?;
    if setup.elevated() {
        return Ok("elevated");
    }
    if let Some(installed) = installed {
        if !notify(0) {
            return Ok("idle");
        }
        setup.launch(Some(installed), notify)?;
        return Ok("opened");
    }
    if !setup.installer_running()? {
        if !notify(1) {
            return Ok("idle");
        }
        setup.download(notify)?;
        if !notify(0) {
            return Ok("idle");
        }
        if setup.installed()?.is_some() || setup.running()? {
            return Ok("ready");
        }
        if !notify(2) {
            return Ok("idle");
        }
        setup.launch(None, notify)?;
    }
    // The bootstrapper owns its process and login. Wait for detection without stopping it.
    if !notify(2) {
        return Ok("idle");
    }
    let deadline = Instant::now() + Duration::from_secs(180);
    while setup.wait(notify) {
        if setup.installed()?.is_some() || setup.running()? {
            return Ok("installed");
        }
        if Instant::now() >= deadline {
            return Ok("unconfirmed");
        }
    }
    Ok("idle")
}

struct WindowsSetup<'a> {
    path: PathBuf,
    preferred: Option<&'a Path>,
    session: u32,
}

impl Setup for WindowsSetup<'_> {
    fn running(&self) -> io::Result<bool> {
        Ok(source::verified_processes()
            .map_err(io::Error::other)?
            .into_iter()
            .any(|(process, _)| processes::in_session(process.pid, self.session)))
    }

    fn installed(&self) -> io::Result<Option<Installation>> {
        if let Some(path) = source::desktop_executable(self.preferred).map_err(io::Error::other)? {
            return Ok(Some(Installation::Desktop(path)));
        }
        let found = !store_paths().map_err(io::Error::other)?.is_empty();
        Ok(found.then_some(Installation::Store))
    }

    fn elevated(&self) -> bool {
        unsafe { IsUserAnAdmin().as_bool() }
    }

    fn installer_running(&self) -> io::Result<bool> {
        processes::running_installer(&self.path, self.session).map_err(io::Error::other)
    }

    fn download(&self, notify: &mut dyn FnMut(u32) -> bool) -> io::Result<()> {
        let directory = self.path.parent().unwrap();
        plain(directory)?;
        fs::create_dir_all(directory)?;
        let partial = self.path.with_extension("part");
        remove_plain(&self.path)?;
        remove_plain(&partial)?;
        let deadline = Instant::now() + Duration::from_secs(90);
        let result = (|| {
            let (mut request, length) = Download::open()?;
            let mut target = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&partial)?;
            copy_setup(&mut request, &mut target, length, || {
                notify(0) && Instant::now() <= deadline
            })?;
            target.flush()?;
            drop(target);
            mark_internet_zone(&partial)?;
            fs::rename(&partial, &self.path)
        })();
        let _ = remove_plain(&partial);
        result
    }

    fn launch(
        &self,
        choice: Option<Installation>,
        notify: &mut dyn FnMut(u32) -> bool,
    ) -> io::Result<()> {
        match choice {
            Some(Installation::Store) if notify(0) => shell(OsStr::new("spotify:")),
            Some(Installation::Store) => Err(cancelled()),
            Some(Installation::Desktop(path)) => open_verified(&path, notify),
            None => open_verified(&self.path, notify),
        }
    }

    fn wait(&self, notify: &mut dyn FnMut(u32) -> bool) -> bool {
        for _ in 0..20 {
            if !notify(0) {
                return false;
            }
            sleep(Duration::from_millis(100));
        }
        notify(0)
    }
}

fn mark_internet_zone(path: &Path) -> io::Result<()> {
    let mut zone = path.as_os_str().to_os_string();
    zone.push(":Zone.Identifier");
    fs::write(
        zone,
        format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={URL}\r\n"),
    )
}

fn plain(path: &Path) -> io::Result<()> {
    const REPARSE_POINT: u32 = 0x400;
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_attributes() & REPARSE_POINT != 0 => {
            Err(invalid("unexpected setup cache link"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_plain(path: &Path) -> io::Result<()> {
    plain(path)?;
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn cleanup(path: &Path) {
    if plain(path.parent().unwrap()).is_ok() {
        let _ = remove_plain(&path.with_extension("part"));
        let _ = remove_plain(path);
    }
}

fn copy_setup(
    input: &mut impl Read,
    output: &mut impl Write,
    expected: Option<u64>,
    mut continue_transfer: impl FnMut() -> bool,
) -> io::Result<()> {
    if expected.is_some_and(|size| size == 0 || size > MAX_BYTES) {
        return Err(invalid("invalid Spotify installer size"));
    }
    let mut total = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if !continue_transfer() {
            return Err(cancelled());
        }
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > MAX_BYTES || expected.is_some_and(|size| total > size) {
            return Err(invalid("Spotify download exceeded its size limit"));
        }
        output.write_all(&buffer[..read])?;
    }
    if total == 0 || expected.is_some_and(|size| total != size) {
        return Err(invalid("incomplete Spotify installer"));
    }
    Ok(())
}

struct Internet(*mut c_void);

impl Internet {
    fn new(handle: *mut c_void) -> io::Result<Self> {
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for Internet {
    fn drop(&mut self) {
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

struct Download {
    request: Internet,
    _connection: Internet,
    _session: Internet,
}

impl Download {
    fn open() -> io::Result<(Self, Option<u64>)> {
        unsafe {
            let session = Internet::new(WinHttpOpen(
                w!("Lummix"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ))?;
            WinHttpSetTimeouts(session.0, 15_000, 15_000, 15_000, 15_000)
                .map_err(io::Error::other)?;
            let connection =
                Internet::new(WinHttpConnect(session.0, w!("download.scdn.co"), 443, 0))?;
            let request = Internet::new(WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                w!("/SpotifySetup.exe"),
                PCWSTR::null(),
                PCWSTR::null(),
                null(),
                WINHTTP_FLAG_SECURE,
            ))?;
            let disabled = WINHTTP_DISABLE_REDIRECTS
                | WINHTTP_DISABLE_COOKIES
                | WINHTTP_DISABLE_AUTHENTICATION;
            WinHttpSetOption(
                Some(request.0),
                WINHTTP_OPTION_DISABLE_FEATURE,
                Some(&disabled.to_ne_bytes()),
            )
            .map_err(io::Error::other)?;
            WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(io::Error::other)?;
            WinHttpReceiveResponse(request.0, null_mut()).map_err(io::Error::other)?;
            let mut code = 0u32;
            let mut size = 4;
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&mut code as *mut u32).cast()),
                &mut size,
                null_mut(),
            )
            .map_err(io::Error::other)?;
            if code != 200 {
                return Err(invalid("unexpected Spotify download HTTP status"));
            }
            let mut length = [0u16; 32];
            let mut size = (length.len() * 2) as u32;
            let expected = match WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_CONTENT_LENGTH,
                PCWSTR::null(),
                Some(length.as_mut_ptr().cast()),
                &mut size,
                null_mut(),
            ) {
                Ok(()) => {
                    let end = length
                        .iter()
                        .position(|ch| *ch == 0)
                        .unwrap_or(length.len());
                    let raw = String::from_utf16(&length[..end]).map_err(io::Error::other)?;
                    if raw.is_empty() || !raw.bytes().all(|ch| ch.is_ascii_digit()) {
                        return Err(invalid("invalid Spotify installer length"));
                    }
                    Some(raw.parse::<u64>().map_err(io::Error::other)?)
                }
                Err(error)
                    if error.code().0 as u32 == (0x8007_0000 | ERROR_WINHTTP_HEADER_NOT_FOUND) =>
                {
                    None
                }
                Err(error) => return Err(io::Error::other(error)),
            };
            Ok((
                Self {
                    request,
                    _connection: connection,
                    _session: session,
                },
                expected,
            ))
        }
    }
}

impl Read for Download {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut read = 0;
        unsafe {
            WinHttpReadData(
                self.request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            )
        }
        .map_err(io::Error::other)?;
        Ok(read as usize)
    }
}

fn verify_publisher(path: &Path, notify: &mut dyn FnMut(u32) -> bool) -> io::Result<()> {
    let mut buffer = [0u16; 32768];
    let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(invalid("Windows system directory is unavailable"));
    }
    let system = PathBuf::from(OsString::from_wide(&buffer[..length]));
    let shell = system.join("WindowsPowerShell/v1.0/powershell.exe");
    let mut child = Command::new(&shell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            CHECK_SIGNATURE,
        ])
        .env("PSModulePath", shell.parent().unwrap().join("Modules"))
        .env("LUMMIX_SPOTIFY_FILE", path)
        .current_dir(&system)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let result = loop {
        if !notify(0) {
            break Err(cancelled());
        }
        if Instant::now() >= deadline {
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Spotify publisher verification timed out",
            ));
        }
        match child.try_wait() {
            Ok(Some(exit)) if exit.success() => return Ok(()),
            Ok(Some(_)) => {
                return Err(invalid(
                    "Windows could not verify Spotify AB as the publisher",
                ))
            }
            Ok(None) => sleep(Duration::from_millis(50)),
            Err(error) => break Err(error),
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).share_mode(1).open(path)
}

fn open_verified(path: &Path, notify: &mut dyn FnMut(u32) -> bool) -> io::Result<()> {
    let _locked = lock_file(path)?;
    verify_publisher(path, notify)?;
    if !notify(0) {
        return Err(cancelled());
    }
    shell(path.as_os_str())
}

fn shell(path: &OsStr) -> io::Result<()> {
    let path: Vec<_> = path.encode_wide().chain([0]).collect();
    let result = unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err(io::Error::other("Windows could not open Spotify"))
    } else {
        Ok(())
    }
}

pub(super) fn run(
    op: u32,
    root: &Path,
    preferred: Option<&Path>,
    notify: &mut dyn FnMut(u32) -> bool,
) -> io::Result<Value> {
    if !root.is_absolute() || root.as_os_str().encode_wide().any(|ch| ch == 0) {
        return Err(invalid("invalid Spotify setup location"));
    }
    let path = root.join("Spotify setup").join("SpotifySetup.exe");
    if op == 0 {
        cleanup(&path);
        return Ok(Value::Null);
    }
    if op == 3 {
        plain(path.parent().unwrap())?;
        plain(&path)?;
        let _locked = lock_file(&path)?;
        verify_publisher(&path, notify)?;
        return Ok(json!(true));
    }
    let session = processes::current_session().map_err(io::Error::other)?;
    let setup = WindowsSetup {
        path,
        preferred,
        session,
    };
    if op == 2 {
        let running = setup.running()?;
        let installation = match setup.installed()? {
            Some(Installation::Desktop(path)) => json!({"edition": "desktop", "path": path}),
            Some(Installation::Store) => json!({"edition": "store"}),
            None => Value::Null,
        };
        return Ok(json!({"running": running, "installation": installation}));
    }
    if op != 1 {
        return Err(invalid("invalid Spotify setup operation"));
    }
    let result = prepare(&setup, notify);
    cleanup(&setup.path);
    match result {
        Ok(phase) => Ok(json!(phase)),
        Err(error) if error.kind() == io::ErrorKind::Interrupted && !notify(0) => Ok(json!("idle")),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
    };

    #[derive(Default)]
    struct Fixture {
        player_running: bool,
        bootstrap_running: bool,
        elevated: bool,
        unavailable: bool,
        installs: RefCell<VecDeque<Option<Installation>>>,
        downloaded: Cell<bool>,
        launched: RefCell<Vec<Option<Installation>>>,
    }

    impl Setup for Fixture {
        fn running(&self) -> io::Result<bool> {
            Ok(self.player_running)
        }
        fn installed(&self) -> io::Result<Option<Installation>> {
            if self.unavailable {
                return Err(invalid("installation identity unavailable"));
            }
            Ok(self.installs.borrow_mut().pop_front().flatten())
        }
        fn elevated(&self) -> bool {
            self.elevated
        }
        fn installer_running(&self) -> io::Result<bool> {
            Ok(self.bootstrap_running)
        }
        fn download(&self, _: &mut dyn FnMut(u32) -> bool) -> io::Result<()> {
            assert!(!self.downloaded.replace(true));
            Ok(())
        }
        fn launch(
            &self,
            choice: Option<Installation>,
            _: &mut dyn FnMut(u32) -> bool,
        ) -> io::Result<()> {
            self.launched.borrow_mut().push(choice);
            Ok(())
        }
        fn wait(&self, notify: &mut dyn FnMut(u32) -> bool) -> bool {
            notify(0)
        }
    }

    #[test]
    fn reuses_both_editions_without_download_and_requires_known_non_elevated_identity() {
        let already_running = Fixture {
            player_running: true,
            unavailable: true,
            ..Default::default()
        };
        assert_eq!(prepare(&already_running, &mut |_| true).unwrap(), "ready");
        for installation in [
            Installation::Desktop(PathBuf::from(r"C:\Spotify.exe")),
            Installation::Store,
        ] {
            let fixture = Fixture {
                installs: RefCell::new(VecDeque::from([Some(installation)])),
                ..Default::default()
            };
            assert_eq!(prepare(&fixture, &mut |_| true).unwrap(), "opened");
            assert!(!fixture.downloaded.get());
            assert_eq!(fixture.launched.borrow().len(), 1);
            assert!(fixture.launched.borrow()[0].is_some());
        }
        let mut fixture = Fixture {
            elevated: true,
            ..Default::default()
        };
        assert_eq!(prepare(&fixture, &mut |_| true).unwrap(), "elevated");
        fixture.unavailable = true;
        assert!(prepare(&fixture, &mut |_| true).is_err());
        assert!(!fixture.downloaded.get());
        assert!(fixture.launched.borrow().is_empty());
    }

    #[test]
    fn launches_one_bootstrapper_and_confirms_detection_or_waits_for_existing_process() {
        for existing in [false, true] {
            let mut installs = VecDeque::from([None]);
            if !existing {
                installs.push_back(None);
            }
            installs.push_back(Some(Installation::Store));
            let fixture = Fixture {
                bootstrap_running: existing,
                installs: RefCell::new(installs),
                ..Default::default()
            };
            assert_eq!(prepare(&fixture, &mut |_| true).unwrap(), "installed");
            assert_eq!(fixture.downloaded.get(), !existing);
            assert_eq!(fixture.launched.borrow().len(), usize::from(!existing));
        }
    }

    #[test]
    fn bounds_download_rejects_truncation_and_cancellation() {
        for length in [0, 3, MAX_BYTES + 1] {
            assert!(copy_setup(&mut &b"MZ"[..], &mut Vec::new(), Some(length), || true).is_err());
        }
        assert!(copy_setup(&mut &b""[..], &mut Vec::new(), None, || true).is_err());
        let err = copy_setup(&mut &b"MZ"[..], &mut Vec::new(), None, || false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        let mut output = Vec::new();
        copy_setup(&mut &b"MZ"[..], &mut output, Some(2), || true).unwrap();
        assert_eq!(output, b"MZ");
    }

    #[test]
    #[ignore = "downloads and verifies the official installer to temporary storage without launching"]
    fn vendor_download_is_bounded_marked_and_signed() {
        let root =
            std::env::temp_dir().join(format!("lummix-spotify-setup-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("SpotifySetup.exe");
        let result = (|| {
            let fixture = WindowsSetup {
                path: path.clone(),
                preferred: None,
                session: 0,
            };
            fixture.download(&mut |_| true)?;
            let mut stream = path.as_os_str().to_os_string();
            stream.push(":Zone.Identifier");
            assert_eq!(
                fs::read_to_string(stream)?,
                format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={URL}\r\n")
            );
            let _locked = lock_file(&path)?;
            verify_publisher(&path, &mut |_| true)
        })();
        cleanup(&path);
        fs::remove_dir(root).unwrap();
        result.unwrap();
    }
}
