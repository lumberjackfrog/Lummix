use super::processes::{matches_creation, process_tree};
use serde_json::{json, Value};
use std::collections::HashSet;
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

unsafe fn session_state(device: &IMMDevice, tree: &HashSet<u32>) -> Result<(bool, bool)> {
    let sessions = audio_sessions(device)?;
    let mut inactive = false;
    for index in 0..sessions.GetCount()? {
        let control: IAudioSessionControl2 = sessions.GetSession(index)?.cast()?;
        if tree.contains(&control.GetProcessId()?) {
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

pub(super) fn direct_to_cable(root_pid: u32, signed: &[String]) -> Result<Value> {
    if signed.is_empty() {
        return Ok(Value::Null);
    }
    let Some(tree) = process_tree(root_pid)? else {
        return Ok(Value::Null);
    };
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
        Ok(json!(inspect_routes(&devices, |device| session_state(
            device, &tree
        ))?))
    }
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
}
