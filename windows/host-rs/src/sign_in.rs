use std::env;
use std::ffi::OsString;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

use winreg::enums::{
    HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_EXPAND_SZ, REG_SZ,
};
use winreg::types::FromRegValue;
use winreg::{RegKey, RegValue};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVAL: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const NAME: &str = "Lummix";

#[derive(Debug, PartialEq, Eq)]
enum State {
    Enabled,
    Disabled,
    Unavailable,
    Occupied,
    Blocked,
}

impl State {
    fn token(&self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Unavailable => "unavailable",
            Self::Occupied => "occupied",
            Self::Blocked => "blocked",
        }
    }
}

struct Startup<'a> {
    hive: &'a RegKey,
    run: &'a str,
    approval: &'a str,
    executable: PathBuf,
    install_root: PathBuf,
}

fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.components()
        .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
        .eq(right
            .components()
            .map(|part| part.as_os_str().to_string_lossy().to_lowercase()))
}

fn lummix_executable(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("lummix.exe"))
}

fn release_folder(path: &Path, root: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Some(parent_root) = parent.parent() else {
        return false;
    };
    let Some(name) = parent.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let fields: Vec<_> = name.split('.').collect();
    same_path(parent_root, root)
        && fields.len() == 3
        && fields
            .iter()
            .all(|field| !field.is_empty() && field.bytes().all(|b| b.is_ascii_digit()))
}

fn target(value: &str) -> &Path {
    if let Some((path, suffix)) = value
        .strip_prefix('"')
        .and_then(|quoted| quoted.split_once('"'))
    {
        if suffix.is_empty() || suffix == " --startup" {
            return Path::new(path);
        }
    }
    Path::new(value)
}

impl Startup<'_> {
    fn read(&self, key: &str) -> io::Result<Option<RegValue>> {
        optional(
            self.hive
                .open_subkey_with_flags(key, KEY_READ)
                .and_then(|key| key.get_raw_value(NAME)),
        )
    }

    fn owns(&self, value: &RegValue) -> bool {
        if !matches!(value.vtype, REG_SZ | REG_EXPAND_SZ) {
            return false;
        }
        let Ok(text) = String::from_reg_value(value) else {
            return false;
        };
        let target = target(&text);
        same_path(target, &self.executable)
            || (lummix_executable(target)
                && release_folder(target, &self.install_root)
                && release_folder(&self.executable, &self.install_root))
    }

    fn state(&self) -> io::Result<State> {
        if !lummix_executable(&self.executable) {
            return Ok(State::Unavailable);
        }
        let Some(value) = self.read(self.run)? else {
            return Ok(State::Disabled);
        };
        if !self.owns(&value) {
            return Ok(State::Occupied);
        }
        if self.read(self.approval)?.is_some_and(|entry| {
            entry.vtype == REG_BINARY && matches!(entry.bytes.first(), Some(3 | 7))
        }) {
            return Ok(State::Blocked);
        }
        Ok(State::Enabled)
    }

    fn change(&self, enabled: bool) -> io::Result<State> {
        if !lummix_executable(&self.executable) {
            return Ok(State::Unavailable);
        }
        let value = self.read(self.run)?;
        if value.as_ref().is_some_and(|value| !self.owns(value)) {
            return Ok(State::Occupied);
        }
        if enabled {
            let (key, _) = self
                .hive
                .create_subkey_with_flags(self.run, KEY_SET_VALUE)?;
            let command = format!("\"{}\" --startup", self.executable.display());
            key.set_value(NAME, &command)?;
        } else if value.is_some() {
            let key = self.hive.open_subkey_with_flags(self.run, KEY_SET_VALUE)?;
            optional(key.delete_value(NAME))?;
        }
        self.state()
    }

    fn refresh(&self) -> io::Result<()> {
        if self
            .read(self.run)?
            .as_ref()
            .is_some_and(|value| self.owns(value))
        {
            self.change(true)?;
        }
        Ok(())
    }
}

pub fn run(args: &[OsString]) -> io::Result<()> {
    let [mode, exe] = args else {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "Missing sign-in arguments",
        ));
    };
    let profile = env::var_os("LOCALAPPDATA")
        .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "Missing LOCALAPPDATA"))?;
    let hive = RegKey::predef(HKEY_CURRENT_USER);
    let startup = Startup {
        hive: &hive,
        run: RUN,
        approval: APPROVAL,
        executable: PathBuf::from(exe),
        install_root: PathBuf::from(profile).join("Programs").join(NAME),
    };
    let state = match mode.to_str() {
        Some("state") => startup.state()?,
        Some("enable") => startup.change(true)?,
        Some("disable") => startup.change(false)?,
        Some("refresh") => return startup.refresh(),
        _ => {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "Unknown sign-in command",
            ))
        }
    };
    writeln!(io::stdout().lock(), "{}", state.token())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn startup_owns_installed_versions_and_preserves_other_entries() {
        let current = Path::new(r"C:\Users\fixture\Programs\Lummix\1.0.9\Lummix.exe");
        let install_root = Path::new(r"C:\Users\fixture\Programs\Lummix");
        let hive = RegKey::predef(HKEY_CURRENT_USER);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let fixture = format!(r"Software\LummixRustHostFixture-{nonce}");
        let (root, _) = hive.create_subkey(&fixture).unwrap();
        let startup = Startup {
            hive: &root,
            run: "Run",
            approval: "Approval",
            executable: current.to_owned(),
            install_root: install_root.to_owned(),
        };
        assert_eq!(startup.state().unwrap(), State::Disabled);
        let (run, _) = root.create_subkey("Run").unwrap();
        run.set_value(
            NAME,
            &r#""c:/users/fixture/programs/lummix/1.0.8/LUMMIX.EXE" --startup"#,
        )
        .unwrap();
        assert_eq!(startup.state().unwrap(), State::Enabled);
        startup.refresh().unwrap();
        assert_eq!(
            run.get_value::<String, _>(NAME).unwrap(),
            format!("\"{}\" --startup", current.display())
        );
        let (approval, _) = root.create_subkey("Approval").unwrap();
        for flag in [3, 7] {
            approval
                .set_raw_value(
                    NAME,
                    &RegValue {
                        bytes: vec![flag],
                        vtype: REG_BINARY,
                    },
                )
                .unwrap();
            assert_eq!(startup.state().unwrap(), State::Blocked);
        }
        assert_eq!(startup.change(false).unwrap(), State::Disabled);
        approval.delete_value(NAME).unwrap();
        assert_eq!(startup.change(true).unwrap(), State::Enabled);
        run.set_value(NAME, &r#""C:\Different program\Lummix.exe" --startup"#)
            .unwrap();
        assert_eq!(startup.state().unwrap(), State::Occupied);
        assert_eq!(startup.change(false).unwrap(), State::Occupied);
        startup.refresh().unwrap();
        assert!(run
            .get_value::<String, _>(NAME)
            .unwrap()
            .contains("Different program"));
        drop((run, approval, root));
        hive.delete_subkey_all(&fixture).unwrap();
    }
}
