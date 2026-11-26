use crate::{vr_catalog as catalog, vr_discovery};
use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::{FreeLibrary, HMODULE},
    System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    },
};

// From Valve OpenVR v1.23.7, headers/openvr_capi.h, IVRSystem_022.
const BACKGROUND: i32 = 3;
const GET_CLASS: usize = 19;
const IS_CONNECTED: usize = 20;
const GET_STRING: usize = 27;
const TRACKING_SYSTEM: i32 = 1000;
const MODEL_NUMBER: i32 = 1001;
const MAX_DEVICES: u32 = 64;

type Init = unsafe extern "C" fn(*mut i32, i32) -> u32;
type Interface = unsafe extern "C" fn(*const u8, *mut i32) -> *const c_void;
type Finish = unsafe extern "C" fn();
type DeviceClass = unsafe extern "system" fn(u32) -> i32;
type Connected = unsafe extern "system" fn(u32) -> u8;
type Property = unsafe extern "system" fn(u32, i32, *mut u8, u32, *mut i32) -> u32;

struct Library(HMODULE);

impl Drop for Library {
    fn drop(&mut self) {
        unsafe { FreeLibrary(self.0) };
    }
}

struct Session(Finish);

impl Drop for Session {
    fn drop(&mut self) {
        unsafe { (self.0)() };
    }
}

fn property(read: Property, device: u32, property: i32) -> String {
    let mut bytes = [0u8; 256];
    let mut error = 0;
    let size = unsafe {
        read(
            device,
            property,
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            &mut error,
        )
    };
    if error != 0 || size as usize > bytes.len() {
        return String::new();
    }
    let length = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..length]).into_owned()
}

fn tracked_label(class: i32, model: &str, tracking: &str) -> Option<&'static str> {
    let default = catalog::TRACKED.get(class.checked_sub(1)? as usize)?;
    let source = tracking.to_lowercase();
    let actual = model.to_lowercase();
    if matches!(source.as_str(), "null" | "simulation" | "simulator")
        || ["simulator", "nullhmd", "null driver"]
            .iter()
            .any(|text| actual.contains(text))
    {
        return None;
    }
    Some(catalog::model(model).unwrap_or(default))
}

pub fn read_devices() -> Option<Vec<&'static str>> {
    // Check before even loading Valve's client. Background initialization will
    // refuse startup if the already-running vrserver disappears during lookup.
    if !cfg!(target_pointer_width = "64") || !vr_discovery::processes().ok()?.contains("vrserver") {
        return None;
    }
    let path = vr_discovery::steam_root().join("steamapps/common/SteamVR/bin/win64/openvr_api.dll");
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let module = unsafe {
        LoadLibraryExW(
            wide.as_ptr(),
            null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    };
    if module.is_null() {
        return None;
    }
    let dll = Library(module);
    // Each export and slot is typed against the versioned Valve C ABI above.
    let (start, interface, finish): (Init, Interface, Finish) = unsafe {
        (
            std::mem::transmute(GetProcAddress(dll.0, c"VR_InitInternal".as_ptr().cast())?),
            std::mem::transmute(GetProcAddress(
                dll.0,
                c"VR_GetGenericInterface".as_ptr().cast(),
            )?),
            std::mem::transmute(GetProcAddress(
                dll.0,
                c"VR_ShutdownInternal".as_ptr().cast(),
            )?),
        )
    };
    let mut error = 0;
    unsafe { start(&mut error, BACKGROUND) };
    if error != 0 {
        return None;
    }
    let _session = Session(finish);
    let table = unsafe { interface(c"FnTable:IVRSystem_022".as_ptr().cast(), &mut error) };
    if table.is_null() || error != 0 {
        return None;
    }
    let entries = table.cast::<*const c_void>();
    let (class, connected, string): (DeviceClass, Connected, Property) = unsafe {
        let category = *entries.add(GET_CLASS);
        let presence = *entries.add(IS_CONNECTED);
        let prop = *entries.add(GET_STRING);
        if category.is_null() || presence.is_null() || prop.is_null() {
            return None;
        }
        (
            std::mem::transmute(category),
            std::mem::transmute(presence),
            std::mem::transmute(prop),
        )
    };
    let mut labels = Vec::new();
    for device in 0..MAX_DEVICES {
        if unsafe { connected(device) } == 0 {
            continue;
        }
        let class = unsafe { class(device) };
        if !(1..=4).contains(&class) {
            continue;
        }
        let model = property(string, device, MODEL_NUMBER);
        let tracking = property(string, device, TRACKING_SYSTEM);
        if let Some(label) = tracked_label(class, &model, &tracking) {
            labels.push(label);
        }
    }
    Some(labels)
}

#[no_mangle]
pub unsafe extern "C" fn lummix_openvr_devices(output: *mut u8, capacity: usize) -> usize {
    let Some(labels) = read_devices() else {
        return 0;
    };
    let Ok(data) = serde_json::to_vec(&labels) else {
        return 0;
    };
    vr_discovery::copy_result(&data, output, capacity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_simulators_and_uses_fixed_labels_without_private_models() {
        assert_eq!(tracked_label(1, "Pico 4", "null"), None);
        assert_eq!(tracked_label(1, "nullhmd", "unknown"), None);
        assert_eq!(tracked_label(9, "Valve Index", "lighthouse"), None);
        assert_eq!(
            tracked_label(2, "Private model 12345", "lighthouse"),
            Some("SteamVR controller")
        );
        assert_eq!(
            tracked_label(3, "Vive tracker 3.0 raw", "lighthouse"),
            Some("VIVE Tracker 3.0")
        );
    }
}
