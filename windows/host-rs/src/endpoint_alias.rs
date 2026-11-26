use std::collections::BTreeSet;
use std::mem::{size_of, zeroed};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::ptr::{null, null_mut};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use windows::core::{BSTR, GUID, PWSTR};
use windows::Win32::Foundation::{HWND as UiWindow, PROPERTYKEY};
use windows::Win32::Media::Audio::{
    eCapture, eRender, EDataFlow, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE,
    DEVICE_STATEMASK_ALL, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, WAIT_OBJECT_0};
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, CreateDesktopW, GetThreadDesktop, GetUserObjectInformationW, UOI_NAME,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, GetCurrentThreadId, GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
    CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
};

const ALIAS: &str = "Lummix";
const VENDOR: &str = "VB-Audio Virtual Cable";
const DRIVER: &str = "VBAudioVACWDM";
const DESKTOP_PREFIX: &str = "LummixAudioName_";
const RENDER_NAMES: [&str; 3] = [
    "CABLE Input (VB-Audio Virtual Cable)",
    "Speakers (VB-Audio Virtual Cable)",
    "CABLE In 16 Ch (VB-Audio Virtual Cable)",
];
const DEVICE: GUID = GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0);
const FRIENDLY: PROPERTYKEY = PROPERTYKEY {
    fmtid: DEVICE,
    pid: 14,
};
const DESC: PROPERTYKEY = PROPERTYKEY {
    fmtid: DEVICE,
    pid: 2,
};
const ADAPTER: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xb3f8fa53_0004_438e_9003_51a46e139bfc),
    pid: 6,
};
const HW: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa8b865dd_2e3d_4094_ad97_e593a70c75d6),
    pid: 8,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AliasError {
    Unavailable = 1,
    Collision = 2,
    NotApplied = 3,
    TimedOut = 4,
}

impl From<windows::core::Error> for AliasError {
    fn from(_: windows::core::Error) -> Self {
        Self::Unavailable
    }
}

type Result<T> = std::result::Result<T, AliasError>;

struct Apartment;
impl Apartment {
    fn enter() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

#[derive(Debug)]
struct Endpoint {
    id: String,
    name: String,
    short: String,
    adapter: String,
    driver: String,
    active: bool,
}

fn take_pwstr(value: PWSTR) -> String {
    if value.is_null() {
        return String::new();
    }
    unsafe {
        let text = value.to_string().unwrap_or_default();
        CoTaskMemFree(Some(value.as_ptr().cast()));
        text
    }
}

fn prop(store: &IPropertyStore, key: &PROPERTYKEY) -> String {
    unsafe {
        let Ok(mut value) = store.GetValue(key) else {
            return String::new();
        };
        let text = PropVariantToStringAlloc(&value)
            .map(take_pwstr)
            .unwrap_or_default();
        let _ = PropVariantClear(&mut value);
        text
    }
}

fn inventory(flow: EDataFlow, states: DEVICE_STATE) -> Result<Vec<Endpoint>> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let collection = enumerator.EnumAudioEndpoints(flow, states)?;
        let mut rows = Vec::new();
        for index in 0..collection.GetCount()? {
            let device = collection.Item(index)?;
            let store = device.OpenPropertyStore(STGM_READ)?;
            rows.push(Endpoint {
                id: take_pwstr(device.GetId()?).to_lowercase(),
                name: prop(&store, &FRIENDLY),
                short: prop(&store, &DESC),
                adapter: prop(&store, &ADAPTER),
                driver: prop(&store, &HW),
                active: device.GetState()? == DEVICE_STATE_ACTIVE,
            });
        }
        Ok(rows)
    }
}

/// Read the same active endpoint names exposed by the Windows property store
/// without configuring any device or touching its settings.
pub(crate) fn active_endpoint_names() -> Option<Vec<(String, &'static str)>> {
    let _apartment = Apartment::enter().ok()?;
    let mut names = Vec::new();
    for (flow, direction) in [(eRender, "output"), (eCapture, "input")] {
        let rows = inventory(flow, DEVICE_STATE_ACTIVE).ok()?;
        names.extend(rows.into_iter().map(|endpoint| (endpoint.name, direction)));
    }
    Some(names)
}

fn signed_ids(input: &Value, flow: &str) -> BTreeSet<String> {
    input
        .get(flow)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_lowercase)
        .collect()
}

fn qualified<'a>(input: &Value, entries: &'a [Endpoint]) -> Result<(&'a Endpoint, bool)> {
    let capture: Vec<_> = signed_ids(input, "Capture")
        .into_iter()
        .filter(|id| id.starts_with("{0.0.1."))
        .collect();
    if capture.len() != 1 || signed_ids(input, "Render").is_empty() {
        return Err(AliasError::Unavailable);
    }
    let mut rows = entries.iter().filter(|row| {
        row.id == capture[0]
            && row.active
            && row.adapter == VENDOR
            && row.driver == DRIVER
            && !row.short.is_empty()
    });
    let first = rows.next().ok_or(AliasError::Unavailable)?;
    if rows.next().is_some() {
        return Err(AliasError::Unavailable);
    }
    let collision = entries.iter().any(|row| {
        row.id != first.id
            && (row.short.eq_ignore_ascii_case(ALIAS)
                || row.name.eq_ignore_ascii_case(ALIAS)
                || (row.short == first.short && row.adapter == first.adapter))
    });
    Ok((first, collision))
}

fn signed_names(input: &Value) -> Result<Value> {
    let capture = input["Capture"]
        .as_array()
        .filter(|ids| ids.len() == 1)
        .ok_or(AliasError::Unavailable)?;
    let capture = capture[0]
        .as_str()
        .ok_or(AliasError::Unavailable)?
        .to_lowercase();
    let render = signed_ids(input, "Render");
    if render.is_empty() {
        return Err(AliasError::Unavailable);
    }
    let captures = inventory(eCapture, DEVICE_STATE_ACTIVE)?;
    let mut names = captures
        .iter()
        .filter(|row| row.id == capture)
        .map(|row| &row.name);
    let capture_name = names
        .next()
        .filter(|name| !name.is_empty())
        .ok_or(AliasError::Unavailable)?;
    if names.next().is_some() {
        return Err(AliasError::Unavailable);
    }
    let renders = inventory(eRender, DEVICE_STATE_ACTIVE)?;
    let chosen = renders
        .iter()
        .filter(|row| render.contains(&row.id))
        .map(|row| &row.name)
        .min_by_key(|name| {
            (
                name.to_lowercase().contains("16 ch"),
                RENDER_NAMES
                    .iter()
                    .position(|known| *known == name.as_str())
                    .unwrap_or(RENDER_NAMES.len()),
                name.as_str(),
            )
        })
        .ok_or(AliasError::Unavailable)?;
    Ok(json!({"render": chosen, "capture": capture_name}))
}

fn desktop_name() -> String {
    let mut name = [0u16; 128];
    let mut length = 0;
    let success = unsafe {
        GetUserObjectInformationW(
            GetThreadDesktop(GetCurrentThreadId()),
            UOI_NAME,
            name.as_mut_ptr().cast(),
            size_of::<[u16; 128]>() as u32,
            &mut length,
        )
    };
    if success == 0 {
        return String::new();
    }
    let end = name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(name.len());
    String::from_utf16_lossy(&name[..end])
}

fn private_name() -> bool {
    desktop_name()
        .strip_prefix(DESKTOP_PREFIX)
        .is_some_and(|nonce| {
            nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

#[no_mangle]
pub extern "C" fn lummix_cable_private_desktop() -> i32 {
    private_name().into()
}

/// Start only the requested worker with no inherited handles on a newly created, non-visible desktop.
#[no_mangle]
pub unsafe extern "C" fn lummix_cable_run_private(
    executable: *const u16,
    command: *const u16,
    command_len: usize,
    timeout_ms: u32,
    exit: *mut u32,
) -> u32 {
    if executable.is_null()
        || command.is_null()
        || command_len == 0
        || command_len >= 32767
        || exit.is_null()
        || timeout_ms == 0
        || timeout_ms > 60_000
    {
        return 87;
    }
    let mut random = [0u8; 16];
    if BCryptGenRandom(
        null_mut(),
        random.as_mut_ptr(),
        random.len() as u32,
        BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    ) != 0
    {
        return 31;
    }
    let name = format!(
        "{DESKTOP_PREFIX}{}",
        random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let short = wide(&name);
    let complete = wide(&format!("winsta0\\{name}"));
    let desktop = CreateDesktopW(short.as_ptr(), null(), null(), 0, 0x000f01ff, null());
    if desktop.is_null() {
        return windows_sys::Win32::Foundation::GetLastError();
    }
    let mut startup: STARTUPINFOW = zeroed();
    startup.cb = size_of::<STARTUPINFOW>() as u32;
    startup.lpDesktop = complete.as_ptr().cast_mut();
    let mut process: PROCESS_INFORMATION = zeroed();
    let mut cmdline = std::slice::from_raw_parts(command, command_len).to_vec();
    cmdline.push(0);
    let started = CreateProcessW(
        executable,
        cmdline.as_mut_ptr(),
        null(),
        null(),
        0,
        CREATE_NO_WINDOW,
        null(),
        null(),
        &startup,
        &mut process,
    );
    let status = if started == 0 {
        windows_sys::Win32::Foundation::GetLastError()
    } else {
        let status = if WaitForSingleObject(process.hProcess, timeout_ms) != WAIT_OBJECT_0 {
            TerminateProcess(process.hProcess, 1);
            WaitForSingleObject(process.hProcess, 2000);
            1460
        } else if GetExitCodeProcess(process.hProcess, exit) == 0 {
            windows_sys::Win32::Foundation::GetLastError()
        } else {
            0
        };
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
        status
    };
    CloseDesktop(desktop);
    status
}

struct Sound(Child);

impl Sound {
    fn launch() -> Result<Self> {
        let directory =
            PathBuf::from(std::env::var_os("SystemRoot").ok_or(AliasError::Unavailable)?)
                .join("System32");
        Command::new(directory.join("rundll32.exe"))
            .args(["shell32.dll,Control_RunDLL", "mmsys.cpl,,1"])
            .current_dir(directory)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(Self)
            .map_err(|_| AliasError::Unavailable)
    }

    fn dialogs(&self) -> Vec<HWND> {
        struct Search {
            pid: u32,
            dialogs: Vec<HWND>,
        }
        unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> i32 {
            let search = &mut *(data as *mut Search);
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == search.pid && IsWindowVisible(hwnd) != 0 {
                let mut name = [0u16; 32];
                let len = GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32);
                if len == 6 && String::from_utf16_lossy(&name[..len as usize]) == "#32770" {
                    search.dialogs.push(hwnd);
                }
            }
            1
        }
        let mut search = Search {
            pid: self.0.id(),
            dialogs: Vec::new(),
        };
        unsafe { EnumWindows(Some(visit), (&mut search as *mut Search) as LPARAM) };
        search.dialogs
    }

    fn one_dialog(&self, except: Option<HWND>) -> Result<HWND> {
        wait(|| {
            let dialogs: Vec<_> = self
                .dialogs()
                .into_iter()
                .filter(|window| Some(*window) != except)
                .collect();
            (dialogs.len() == 1).then(|| dialogs[0])
        })
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        for hwnd in self.dialogs() {
            unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
        }
        let until = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < until {
            if self.0.try_wait().is_ok_and(|state| state.is_some()) {
                return;
            }
            thread::sleep(Duration::from_millis(40));
        }
        if self.0.kill().is_ok() {
            let _ = self.0.wait();
        }
    }
}

fn wait<T>(mut operation: impl FnMut() -> Option<T>) -> Result<T> {
    let until = Instant::now() + Duration::from_secs(4);
    while Instant::now() < until {
        if let Some(value) = operation() {
            return Ok(value);
        }
        thread::sleep(Duration::from_millis(60));
    }
    Err(AliasError::TimedOut)
}

unsafe fn elements(
    uia: &IUIAutomation,
    parent: &IUIAutomationElement,
    id: UIA_PROPERTY_ID,
    value: VARIANT,
    scope: TreeScope,
) -> Result<Vec<IUIAutomationElement>> {
    let condition = uia.CreatePropertyCondition(id, &value)?;
    let set = parent.FindAll(scope, &condition)?;
    (0..set.Length()?)
        .map(|index| set.GetElement(index).map_err(Into::into))
        .collect()
}

unsafe fn kind(
    uia: &IUIAutomation,
    parent: &IUIAutomationElement,
    typed: UIA_CONTROLTYPE_ID,
) -> Result<Vec<IUIAutomationElement>> {
    elements(
        uia,
        parent,
        UIA_ControlTypePropertyId,
        typed.0.into(),
        TreeScope_Children,
    )
}

unsafe fn element(
    uia: &IUIAutomation,
    parent: &IUIAutomationElement,
    id: &str,
    typed: UIA_CONTROLTYPE_ID,
) -> Result<IUIAutomationElement> {
    let mut matching = elements(
        uia,
        parent,
        UIA_AutomationIdPropertyId,
        id.into(),
        TreeScope_Descendants,
    )?
    .into_iter()
    .filter(|element| element.CurrentControlType().ok() == Some(typed));
    let first = matching.next().ok_or(AliasError::Unavailable)?;
    if matching.next().is_some() {
        return Err(AliasError::Unavailable);
    }
    Ok(first)
}

unsafe fn invoke(button: IUIAutomationElement) -> Result<()> {
    if !button.CurrentIsEnabled()?.as_bool() {
        return Err(AliasError::Unavailable);
    }
    button
        .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)?
        .Invoke()?;
    Ok(())
}

fn apply(input: &Value) -> Result<()> {
    if !private_name() {
        return Err(AliasError::Unavailable);
    }
    let endpoints = inventory(eCapture, DEVICE_STATE(DEVICE_STATEMASK_ALL))?;
    let (target, collision) = qualified(input, &endpoints)?;
    if input["workerId"].as_str() != Some(&target.id) {
        return Err(AliasError::Unavailable);
    }
    if collision {
        return Err(AliasError::Collision);
    }
    if target.short == ALIAS {
        return Ok(());
    }
    unsafe {
        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)?;
        let sound = Sound::launch()?;
        let main = sound.one_dialog(None)?;
        let dialog = automation.ElementFromHandle(UiWindow(main))?;
        let tabs = element(&automation, &dialog, "12320", UIA_TabControlTypeId)?;
        let tabs = kind(&automation, &tabs, UIA_TabItemControlTypeId)?;
        let capture = tabs.get(1).ok_or(AliasError::Unavailable)?;
        if !capture
            .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)?
            .CurrentIsSelected()?
            .as_bool()
        {
            return Err(AliasError::Unavailable);
        }
        let listing = element(&automation, &dialog, "1000", UIA_ListControlTypeId)?;
        let mut matching = Vec::new();
        for row in kind(&automation, &listing, UIA_ListItemControlTypeId)? {
            if row.CurrentName()?.to_string() == target.short
                && kind(&automation, &row, UIA_TextControlTypeId)?
                    .iter()
                    .any(|text| {
                        text.CurrentName()
                            .is_ok_and(|name| name.to_string() == target.adapter)
                    })
            {
                matching.push(row);
            }
        }
        if matching.len() > 1 {
            return Err(AliasError::Collision);
        }
        matching
            .first()
            .ok_or(AliasError::Unavailable)?
            .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)?
            .Select()?;
        invoke(element(
            &automation,
            &dialog,
            "1003",
            UIA_ButtonControlTypeId,
        )?)?;
        let properties = sound.one_dialog(Some(main))?;
        let sheet = automation.ElementFromHandle(UiWindow(properties))?;
        let adapter = element(&automation, &sheet, "1207", UIA_TextControlTypeId)?;
        let edit = element(&automation, &sheet, "1201", UIA_EditControlTypeId)?
            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)?;
        if edit.CurrentValue()?.to_string() != target.short
            || adapter.CurrentName()?.to_string() != target.adapter
            || edit.CurrentIsReadOnly()?.as_bool()
        {
            return Err(AliasError::Unavailable);
        }
        edit.SetValue(&BSTR::from(ALIAS))?;
        if edit.CurrentValue()?.to_string() != ALIAS {
            return Err(AliasError::NotApplied);
        }
        invoke(element(&automation, &sheet, "1", UIA_ButtonControlTypeId)?)?;
        wait(|| (!sound.dialogs().contains(&properties)).then_some(()))?;
    }
    Ok(())
}

type Output = extern "C" fn(*const u8, usize);

fn run(operation: u32, data: &[u8]) -> Result<Value> {
    if operation == 2 && !private_name() {
        return Err(AliasError::Unavailable);
    }
    let input = serde_json::from_slice::<Value>(data).map_err(|_| AliasError::Unavailable)?;
    let _apartment = Apartment::enter()?;
    match operation {
        0 => {
            let rows = inventory(eCapture, DEVICE_STATE(DEVICE_STATEMASK_ALL))?;
            let (row, collision) = qualified(&input, &rows)?;
            Ok(
                json!({"id":row.id,"name":row.name,"short":row.short,"adapter":row.adapter,"collision":collision}),
            )
        }
        1 => signed_names(&input),
        2 => {
            apply(&input)?;
            Ok(Value::Null)
        }
        _ => Err(AliasError::Unavailable),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lummix_cable_endpoint(
    operation: u32,
    input: *const u8,
    length: usize,
    output: Option<Output>,
) -> u32 {
    if input.is_null() || length == 0 || length > 16_384 {
        return AliasError::Unavailable as u32;
    }
    match run(operation, std::slice::from_raw_parts(input, length)) {
        Ok(value) => {
            if let Some(output) = output {
                if let Ok(data) = serde_json::to_vec(&value) {
                    output(data.as_ptr(), data.len());
                }
            }
            0
        }
        Err(error) => error as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, short: &str, active: bool) -> Endpoint {
        Endpoint {
            id: id.into(),
            name: format!("{short} ({VENDOR})"),
            short: short.into(),
            adapter: VENDOR.into(),
            driver: DRIVER.into(),
            active,
        }
    }

    #[test]
    fn only_one_signed_active_capture_with_exact_adapter_and_driver_is_eligible() {
        let id = "{0.0.1.00000000}.{signed}";
        let input = json!({"Capture":[id.to_uppercase()],"Render":["{0.0.0.00000000}.{signed}"]});
        let rows = [row(id, "CABLE Output", true)];
        let (matched, collision) = qualified(&input, &rows).unwrap();
        assert_eq!(matched.id, id);
        assert!(!collision);
        let rows = [row(id, "CABLE Output", false)];
        assert!(matches!(
            qualified(&input, &rows),
            Err(AliasError::Unavailable)
        ));
        let mut wrong_driver = row(id, "CABLE Output", true);
        wrong_driver.driver = "OtherDriver".into();
        assert!(matches!(
            qualified(&input, &[wrong_driver]),
            Err(AliasError::Unavailable)
        ));
        for entries in [
            [
                row(id, "CABLE Output", true),
                row("another-capture", "CABLE Output", false),
            ],
            [
                row(id, "CABLE Output", true),
                row("another-capture", "lummix", false),
            ],
        ] {
            assert!(qualified(&input, &entries).unwrap().1);
        }
        let many = json!({"Capture":[id,"{0.0.1.00000000}.{second}"],"Render":["signed"]});
        assert!(matches!(
            qualified(&many, &rows),
            Err(AliasError::Unavailable)
        ));
    }

    #[test]
    fn native_alias_refuses_the_visible_desktop_without_opening_sound() {
        assert!(!private_name());
        assert_eq!(
            run(2, br#"{"Capture":[],"Render":[]}"#),
            Err(AliasError::Unavailable)
        );
    }

    #[test]
    fn native_private_launcher_sets_a_distinct_worker_desktop() {
        let before = desktop_name();
        assert!(!before.is_empty());
        let exe = std::env::current_exe().unwrap();
        let line = wide(&format!(
            "\"{}\" --exact endpoint_alias::tests::assert_private_worker_desktop --ignored",
            exe.display()
        ));
        let name = wide(&exe.to_string_lossy());
        let mut status = 0;
        let result = unsafe {
            lummix_cable_run_private(
                name.as_ptr(),
                line.as_ptr(),
                line.len() - 1,
                5000,
                &mut status,
            )
        };
        assert_eq!(result, 0);
        assert_eq!(status, 0);
        assert_eq!(desktop_name(), before);
    }

    #[test]
    #[ignore = "spawned by the native private desktop test, never run on the user's desktop"]
    fn assert_private_worker_desktop() {
        assert!(private_name());
    }
}
