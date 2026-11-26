use serde_json::{json, Value};
use windows::{
    core::{w, Error, Result},
    Win32::{
        Foundation::{
            ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND,
            ERROR_SUCCESS, ERROR_UNSUPPORTED_TYPE,
        },
        System::Registry::{
            RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_NOEXPAND,
            RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6432KEY, RRF_SUBKEY_WOW6464KEY,
        },
    },
};

pub fn desktop_registrations() -> Result<Value> {
    // Windows path length bound. A longer registry entry cannot be a usable executable path.
    let mut buffer = vec![0u16; 32768];
    let app_paths = w!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\Spotify.exe");
    let uninstall = w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Spotify");
    let entries = [
        (app_paths, w!(""), ""),
        (uninstall, w!("InstallLocation"), "InstallLocation"),
        (uninstall, w!("DisplayIcon"), "DisplayIcon"),
    ];
    let mut values = Vec::new();
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for view in [RRF_SUBKEY_WOW6464KEY, RRF_SUBKEY_WOW6432KEY] {
            for (key, name, field) in entries {
                let mut size = (buffer.len() * 2) as u32;
                let status = unsafe {
                    RegGetValueW(
                        hive,
                        key,
                        name,
                        view | RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
                        None,
                        Some(buffer.as_mut_ptr().cast()),
                        Some(&mut size),
                    )
                };
                match status {
                    ERROR_SUCCESS => {
                        let text = &buffer[..size as usize / 2];
                        if let Ok(value) =
                            String::from_utf16(text.strip_suffix(&[0]).unwrap_or(text))
                        {
                            values.push(json!([field, value]));
                        }
                    }
                    ERROR_FILE_NOT_FOUND
                    | ERROR_PATH_NOT_FOUND
                    | ERROR_ACCESS_DENIED
                    | ERROR_UNSUPPORTED_TYPE
                    | ERROR_MORE_DATA => {}
                    _ => return Err(Error::from_hresult(status.to_hresult())),
                }
            }
        }
    }
    Ok(json!(values))
}
