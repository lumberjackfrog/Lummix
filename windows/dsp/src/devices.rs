use super::{c_int, c_void, Api};
use serde_json::{json, Value};
use std::ffi::{c_char, CStr, CString};
use std::ptr;
use windows::core::{Interface, Result, GUID, PWSTR};
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, PROPERTYKEY};

struct Com(bool);
impl Com {
    fn enter() -> Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            return Ok(Self(false));
        }
        hr.ok()?;
        Ok(Self(true))
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                CoUninitialize();
            }
        }
    }
}
fn take_string(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let value = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0.cast())) };
    value
}
fn prop(store: &IPropertyStore, guid: u128, pid: u32) -> String {
    let key = PROPERTYKEY {
        fmtid: GUID::from_u128(guid),
        pid,
    };
    unsafe {
        store
            .GetValue(&key)
            .and_then(|v| PropVariantToStringAlloc(&v))
            .map(take_string)
            .unwrap_or_default()
    }
}
fn kind(name: &str, hardware: &str, instance: &str, form: &str) -> Option<&'static str> {
    let s = format!("{name} {hardware} {instance}").to_lowercase();
    let has = |list: &[&str]| list.iter().any(|word| s.contains(word));
    if has(&[
        "vbaudio",
        "vb-audio",
        "loopback",
        "stereo mix",
        "what u hear",
        "voicemeeter",
        "steam streaming",
        "steamstreaming",
        "streaming speaker",
        "sonar",
    ]) {
        return None;
    }
    let brand = has(&["pico", "oculus", "quest", "vive", "index", "pimax"]);
    let physical = has(&[
        r"usb\",
        r"hdaudio\",
        r"intelaudio\",
        "bth",
        r"acp\",
        r"amdacp\",
        r"acpi\",
        r"pci\",
        r"soundwire\",
    ]);
    if physical {
        Some(
            if form == "5" || brand || has(&["headset", "headphone", "hands-free"]) {
                "headset"
            } else {
                "physical"
            },
        )
    } else if brand && (s.contains("mic") || matches!(form, "4" | "5")) {
        Some("headsetBridge")
    } else {
        None
    }
}
fn inventory() -> Result<Vec<Value>> {
    let _com = Com::enter()?;
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let default = |role| {
            en.GetDefaultAudioEndpoint(eCapture, role)
                .and_then(|d| d.GetId())
                .map(|s| take_string(s).to_lowercase())
                .unwrap_or_default()
        };
        let (normal, comm) = (default(eConsole), default(eCommunications));
        let devices = en.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let mut rows = Vec::new();
        for i in 0..devices.GetCount()? {
            let d = devices.Item(i)?;
            let s = d.OpenPropertyStore(STGM_READ)?;
            let name = prop(&s, 0xa45c254e_df1c_4efd_8020_67d146a850e0, 14);
            let hw = prop(&s, 0xa8b865dd_2e3d_4094_ad97_e593a70c75d6, 8);
            let instance = prop(&s, 0xb3f8fa53_0004_438e_9003_51a46e139bfc, 2);
            let form = prop(&s, 0x1da5d803_d492_4edd_8c23_e0c0ffee7f0e, 0);
            if let Some(kind) = kind(&name, &hw, &instance, &form) {
                let id = take_string(d.GetId()?).to_lowercase();
                rows.push(json!({"name":name,"id":id,"kind":kind,"headset":kind!="physical","defaultInput":id==normal,
                                 "defaultCommunications":id==comm}));
            }
        }
        rows.sort_by_cached_key(|r| {
            (
                r["name"].as_str().unwrap().to_lowercase(),
                r["id"].as_str().unwrap().to_owned(),
            )
        });
        Ok(rows)
    }
}
#[no_mangle]
pub extern "C" fn lummix_inputs() -> *mut c_char {
    inventory()
        .ok()
        .and_then(|rows| CString::new(serde_json::to_vec(&rows).ok()?).ok())
        .map_or(ptr::null_mut(), CString::into_raw)
}
#[no_mangle]
pub unsafe extern "C" fn lummix_inputs_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}
type Count = unsafe extern "C" fn() -> c_int;
type Endpoint = unsafe extern "C" fn(c_int, *mut *mut c_void) -> c_int;
#[repr(C)]
pub struct Pair {
    mic: c_int,
    sink: c_int,
}
const NO_PAIR: Pair = Pair { mic: -1, sink: -1 };
const RENDERS: [&str; 3] = [
    "CABLE Input (VB-Audio Virtual Cable)",
    "Speakers (VB-Audio Virtual Cable)",
    "CABLE In 16 Ch (VB-Audio Virtual Cable)",
];

#[no_mangle]
pub unsafe extern "C" fn lummix_route_indexes(
    api: &Api,
    count: Count,
    endpoint: Endpoint,
    refresh: Option<Count>,
    input: *const c_char,
    ids: *const c_char,
) -> Pair {
    if input.is_null() || ids.is_null() {
        return NO_PAIR;
    }
    let Ok(input) = unsafe { CStr::from_ptr(input) }.to_str() else {
        return NO_PAIR;
    };
    if !input.starts_with("{0.0.1.") {
        return NO_PAIR;
    }
    let Ok(sinks) =
        serde_json::from_slice::<Vec<String>>(unsafe { CStr::from_ptr(ids).to_bytes() })
    else {
        return NO_PAIR;
    };
    let Ok(_com) = Com::enter() else {
        return NO_PAIR;
    };
    let find = || {
        let mut mic = None;
        let mut selected = None;
        for i in 0..unsafe { count() } {
            let Some(dev) = (unsafe { (api.device)(i).as_ref() }) else {
                continue;
            };
            if unsafe { (api.host)(dev.host).as_ref() }.is_none_or(|h| h.api != 13) {
                continue;
            }
            let mut raw = ptr::null_mut();
            if unsafe { endpoint(i, &mut raw) } != 0 {
                continue;
            }
            // PortAudio owns this interface. The borrow ends before any device-list refresh.
            let Some(d) = (unsafe { IMMDevice::from_raw_borrowed(&raw) }) else {
                continue;
            };
            let Ok(id) = (unsafe { d.GetId() }).map(take_string) else {
                continue;
            };
            if id.eq_ignore_ascii_case(input) && dev.inputs > 0 && mic.replace(i).is_some() {
                return None;
            }
            if dev.outputs < 2 || !sinks.iter().any(|s| s.eq_ignore_ascii_case(&id)) {
                continue;
            }
            let name = if dev.name.is_null() {
                std::borrow::Cow::Borrowed("")
            } else {
                unsafe { CStr::from_ptr(dev.name.cast()) }.to_string_lossy()
            };
            let rank = (
                dev.outputs != 2 || name.to_ascii_lowercase().contains("16 ch"),
                RENDERS
                    .iter()
                    .position(|s| *s == name)
                    .unwrap_or(RENDERS.len()),
                i,
            );
            if selected.is_none_or(|old| rank < old) {
                selected = Some(rank);
            }
        }
        Some(Pair {
            mic: mic?,
            sink: selected?.2,
        })
    };
    find()
        .or_else(|| {
            if refresh.is_some_and(|f| unsafe { f() } == 0) {
                find()
            } else {
                None
            }
        })
        .unwrap_or(NO_PAIR)
}
#[cfg(test)]
mod tests {
    use super::kind;
    #[test]
    fn microphone_identity() {
        assert_eq!(kind("Line", "USB\\Class_01", "", "4"), Some("physical"));
        assert_eq!(kind("Main", "USB\\Class_01", "", "5"), Some("headset"));
        assert_eq!(
            kind("Mic (PicoStreamingMicrophone)", "ROOT\\virtual", "", "4"),
            Some("headsetBridge")
        );
        for hw in [
            "VBAudioVACWDM",
            "ROOT\\SteamStreamingMicrophone",
            "loopback",
        ] {
            assert_eq!(kind("renamed", hw, "", "4"), None);
        }
    }
}
