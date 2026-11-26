use super::processes::{matches_creation, process_tree};
use serde_json::{json, Value};
use std::collections::HashMap;
use windows::{
    core::{Error, Interface, Result, HRESULT},
    Win32::{
        Media::Audio::{
            eRender, AudioSessionStateActive, AudioSessionStateInactive,
            Endpoints::IAudioMeterInformation, IAudioSessionControl2, IAudioSessionEnumerator,
            IAudioSessionManager2, IMMDevice, IMMDeviceCollection, IMMDeviceEnumerator,
            MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
        },
        System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL},
    },
};

unsafe fn render_endpoints() -> Result<IMMDeviceCollection> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
}

unsafe fn audio_sessions(device: &IMMDevice) -> Result<IAudioSessionEnumerator> {
    let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
    manager.GetSessionEnumerator()
}

unsafe fn session_state(device: &IMMDevice, tree: &HashMap<u32, u32>) -> Result<(bool, bool)> {
    let sessions = audio_sessions(device)?;
    let mut inactive = false;
    for index in 0..sessions.GetCount()? {
        let control: IAudioSessionControl2 = sessions.GetSession(index)?.cast()?;
        if tree.contains_key(&control.GetProcessId()?) {
            let state = control.GetState()?;
            if state == AudioSessionStateActive {
                return Ok((true, inactive));
            }
            inactive |= state == AudioSessionStateInactive;
        }
    }
    Ok((false, inactive))
}

fn inspect_routes<T>(
    devices: &[(bool, T)],
    mut probe: impl FnMut(&T) -> Result<(bool, bool)>,
) -> Result<Option<bool>> {
    let mut has_cable = false;
    let mut idle_on_cable = false;
    for (_, device) in devices.iter().filter(|(is_cable, _)| *is_cable) {
        has_cable = true;
        let (active, inactive) = probe(device)?;
        if active {
            return Ok(Some(true));
        }
        idle_on_cable |= inactive;
    }
    if !has_cable {
        return Ok(None);
    }
    if idle_on_cable {
        // An active stream on another endpoint clears a stale inactive cable session.
        for (_, device) in devices.iter().filter(|(is_cable, _)| !is_cable) {
            if probe(device)?.0 {
                return Ok(Some(false));
            }
        }
        return Ok(None);
    }
    Ok(Some(false))
}

pub(super) fn direct_to_cable(root_pid: u32, signed: &[String]) -> Result<Option<bool>> {
    let Some(tree) = process_tree(root_pid)? else {
        return Ok(None);
    };
    direct_to_cable_in_tree(&tree, signed)
}

pub(super) fn direct_to_cable_in_tree(
    tree: &HashMap<u32, u32>,
    signed: &[String],
) -> Result<Option<bool>> {
    if signed.is_empty() {
        return Ok(None);
    }
    unsafe {
        let devices = render_endpoints()?;
        let devices = (0..devices.GetCount()?)
            .map(|index| {
                let device = devices.Item(index)?;
                let id = device.GetId()?;
                let id_string = id.to_string();
                CoTaskMemFree(Some(id.0.cast()));
                let id =
                    id_string.map_err(|_| Error::from_hresult(HRESULT(0x8007000du32 as i32)))?;
                let is_cable = signed
                    .iter()
                    .any(|expected| expected.eq_ignore_ascii_case(&id));
                Ok((is_cable, device))
            })
            .collect::<Result<Vec<_>>>()?;
        inspect_routes(&devices, |device| session_state(device, tree))
    }
}

fn unique_renderer(
    tree: &HashMap<u32, u32>,
    sessions: impl IntoIterator<Item = (u32, bool)>,
) -> Option<u32> {
    let mut selected = None;
    for (pid, single_owner) in sessions {
        if !tree.contains_key(&pid) {
            continue;
        }
        if !single_owner || selected.is_some_and(|previous| previous != pid) {
            return None;
        }
        selected = Some(pid);
    }
    selected
}

pub(super) fn active_renderer(tree: &HashMap<u32, u32>) -> Result<Option<u32>> {
    let mut active = Vec::new();
    unsafe {
        let endpoints = render_endpoints()?;
        for index in 0..endpoints.GetCount()? {
            let sessions = audio_sessions(&endpoints.Item(index)?)?;
            for index in 0..sessions.GetCount()? {
                let control = sessions
                    .GetSession(index)?
                    .cast::<IAudioSessionControl2>()?;
                if control.GetState()? != AudioSessionStateActive {
                    continue;
                }
                // WASAPI may report the creator PID for a cross-process session.
                // Such a session does not uniquely identify a renderer.
                let mut pid = 0;
                let status = (control.vtable().GetProcessId)(control.as_raw(), &mut pid);
                status.ok()?;
                active.push((pid, status.0 == 0));
            }
        }
    }
    Ok(unique_renderer(tree, active))
}

pub(super) fn peak(processes: &[(u32, f64)]) -> Result<Value> {
    if processes.is_empty() {
        return Ok(Value::Null);
    }
    unsafe {
        let endpoints = render_endpoints()?;
        let mut peak: Option<f32> = None;
        for index in 0..endpoints.GetCount()? {
            let Ok(sessions) = endpoints.Item(index).and_then(|dev| audio_sessions(&dev)) else {
                continue;
            };
            let Ok(count) = sessions.GetCount() else {
                continue;
            };
            for index in 0..count {
                let Ok(control) = sessions
                    .GetSession(index)
                    .and_then(|control| control.cast::<IAudioSessionControl2>())
                else {
                    continue;
                };
                if !control.GetProcessId().is_ok_and(|pid| {
                    processes
                        .iter()
                        .any(|(target, created)| *target == pid && matches_creation(pid, *created))
                }) || control.GetState().ok() != Some(AudioSessionStateActive)
                {
                    continue;
                }
                if let Ok(value) = control
                    .cast::<IAudioMeterInformation>()
                    .and_then(|meter| meter.GetPeakValue())
                {
                    if (0.0..=1.0).contains(&value) {
                        peak = Some(peak.map_or(value, |previous| previous.max(value)));
                    }
                }
            }
        }
        Ok(json!(peak))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unverified_or_stale_cable_route() {
        let regular = (false, (true, false));
        let idle_cable = (true, (false, true));
        let direct = (true, (true, false));
        let off = (false, (false, false));
        for (devices, expected) in [
            ([regular, direct], Some(true)),
            ([regular, idle_cable], Some(false)),
            ([off, idle_cable], None),
            ([off, regular], None),
            ([regular, (true, (false, false))], Some(false)),
        ] {
            assert_eq!(
                inspect_routes(&devices, |states| Ok(*states)).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn only_one_active_renderer_in_the_verified_tree_narrows_the_capture() {
        let tree = HashMap::from([(1, 1), (2, 1), (3, 2), (4, 3), (5, 3)]);
        assert_eq!(
            unique_renderer(&tree, [(4, true), (4, true), (17, true)]),
            Some(4)
        );
        assert_eq!(unique_renderer(&tree, [(17, true)]), None);
        assert_eq!(unique_renderer(&tree, [(4, true), (5, true)]), None);
        assert_eq!(unique_renderer(&tree, [(4, false)]), None);
    }
}
