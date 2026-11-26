use super::window_player::{controls, verified_owner};
use serde_json::{json, Value};
use std::{
    thread::sleep,
    time::{Duration, Instant},
};
use windows::{
    core::Result,
    Win32::{
        Foundation::HWND,
        System::{
            Com::{CoCreateInstance, CLSCTX_INPROC_SERVER, SAFEARRAY},
            Ole::{SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound},
        },
        UI::Accessibility::{
            CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern,
            IUIAutomationRangeValuePattern, TreeScope_Children, TreeScope_Descendants,
            UIA_ButtonControlTypeId, UIA_InvokePatternId, UIA_RangeValuePatternId,
            UIA_SliderControlTypeId,
        },
    },
};

fn unavailable(reason: &'static str) -> Value {
    json!({"reason": reason, "http": 409})
}

fn muted(name: &str) -> Option<bool> {
    match name.trim().to_lowercase().as_str() {
        "mute" | "silenciar" | "desactivar el sonido" => Some(false),
        "unmute" | "activar sonido" | "activar el sonido" | "quitar silencio"
        | "dejar de silenciar" => Some(true),
        _ => None,
    }
}

unsafe fn volume_slider(element: &IUIAutomationElement) -> Result<bool> {
    let name = element.CurrentName()?.to_string();
    Ok(matches!(
        name.trim().to_lowercase().as_str(),
        "change volume"
            | "cambiar volumen"
            | "cambiar el volumen"
            | "ajustar volumen"
            | "ajustar el volumen"
    ))
}

struct RuntimeId(*mut SAFEARRAY);
impl Drop for RuntimeId {
    fn drop(&mut self) {
        let _ = unsafe { SafeArrayDestroy(self.0) };
    }
}

unsafe fn runtime_id(element: &IUIAutomationElement) -> Result<Option<Vec<i32>>> {
    let array = element.GetRuntimeId()?;
    if array.is_null() {
        return Ok(None);
    }
    let array = RuntimeId(array);
    let lower = SafeArrayGetLBound(array.0, 1)?;
    let upper = SafeArrayGetUBound(array.0, 1)?;
    if !(0..=100).contains(&(i64::from(upper) - i64::from(lower))) {
        return Ok(None);
    }
    let mut values = Vec::new();
    for index in lower..=upper {
        let mut value = 0i32;
        SafeArrayGetElement(array.0, &index, (&mut value as *mut i32).cast())?;
        values.push(value);
    }
    Ok(Some(values))
}

struct VolumeTarget {
    hwnd: HWND,
    pid: u32,
    id: Value,
    slider: IUIAutomationRangeValuePattern,
    mute: IUIAutomationElement,
}

unsafe fn choose_target(
    ui: &IUIAutomation,
    windows: &[Value],
) -> Result<std::result::Result<VolumeTarget, &'static str>> {
    let mut candidates = Vec::new();
    let walker = ui.ControlViewWalker()?;
    for window in windows {
        let [handle, pid, created] = window.as_array().map(Vec::as_slice).unwrap_or_default()
        else {
            return Ok(Err("selection_changed"));
        };
        let (Some(handle), Some(pid), Some(_)) = (handle.as_u64(), pid.as_u64(), created.as_f64())
        else {
            return Ok(Err("selection_changed"));
        };
        let (hwnd, pid) = (HWND(handle as usize as *mut _), pid as u32);
        if !verified_owner(hwnd, pid) {
            return Ok(Err("selection_changed"));
        }
        let root = ui.ElementFromHandle(hwnd)?;
        for element in controls(ui, &root, TreeScope_Descendants, UIA_SliderControlTypeId)? {
            if !volume_slider(&element)? {
                continue;
            }
            let mut parent = walker.GetParentElement(&element);
            let mut mute = None;
            for _ in 0..3 {
                let Ok(bar) = parent else {
                    break;
                };
                let mut found = Vec::new();
                for button in controls(ui, &bar, TreeScope_Children, UIA_ButtonControlTypeId)? {
                    let name = button.CurrentName()?.to_string();
                    let name = name.trim().to_lowercase();
                    if [
                        "listening on ",
                        "playing on ",
                        "escuchando en ",
                        "reproduciendo en ",
                    ]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
                    {
                        return Ok(Err("connect_device"));
                    }
                    if muted(&name).is_some() {
                        found.push(button);
                    }
                }
                if found.len() == 1 {
                    mute = found.pop();
                    break;
                }
                parent = walker.GetParentElement(&bar);
            }
            let Some(mute) = mute else {
                continue;
            };
            let Ok(slider) = element
                .GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
            else {
                return Ok(Err("slider_unavailable"));
            };
            if !element.CurrentIsEnabled()?.as_bool()
                || slider.CurrentIsReadOnly()?.as_bool()
                || !matches!(
                    (slider.CurrentMinimum()?, slider.CurrentMaximum()?),
                    (0.0, 1.0 | 100.0)
                )
            {
                return Ok(Err("slider_unavailable"));
            }
            let Some(element_id) = runtime_id(&element)? else {
                return Ok(Err("slider_unavailable"));
            };
            let id = json!([handle, pid, created, element_id]);
            candidates.push(VolumeTarget {
                hwnd,
                pid,
                id,
                slider,
                mute,
            });
        }
    }
    Ok(match candidates.len() {
        0 => Err("no_player_slider"),
        1 => Ok(candidates.pop().unwrap()),
        _ => Err("ambiguous_player_slider"),
    })
}

impl VolumeTarget {
    unsafe fn state(&self) -> Result<Option<(f64, bool)>> {
        let value = 100.0 * self.slider.CurrentValue()? / self.slider.CurrentMaximum()?;
        if !(0.0..=100.0).contains(&value) {
            return Ok(None);
        }
        Ok(muted(&self.mute.CurrentName()?.to_string())
            .map(|muted| ((value * 100.0).round_ties_even() / 100.0, muted)))
    }

    fn result(&self, volume: f64, muted: bool) -> Value {
        json!([self.id, {"state": "ready", "source": "spotify_player", "volume": volume, "muted": muted}])
    }
}

enum Update {
    Volume(f64),
    Mute(bool),
}

impl Update {
    fn decode(change: &Value) -> Option<Self> {
        let fields = change.as_object()?;
        if fields.len() != 1 {
            return None;
        }
        if let Some(value) = fields.get("volume").and_then(Value::as_f64) {
            return (0.0..=100.0)
                .contains(&value)
                .then_some(Self::Volume(value));
        }
        fields.get("muted").and_then(Value::as_bool).map(Self::Mute)
    }

    fn matches(&self, volume: f64, muted: bool) -> bool {
        match self {
            Self::Volume(wanted) => (volume - wanted).abs() <= 0.5,
            Self::Mute(wanted) => muted == *wanted,
        }
    }
}

pub(super) fn player_volume(request: &Value) -> Result<Value> {
    let Some(windows) = request.get("windows").and_then(Value::as_array) else {
        return Ok(unavailable("slider_unavailable"));
    };
    unsafe {
        let ui: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
        let target = match choose_target(&ui, windows)? {
            Ok(target) => target,
            Err(reason) => return Ok(unavailable(reason)),
        };
        let change = &request["change"];
        if !change.is_null() && request["expected"] != target.id {
            return Ok(unavailable(if request["expected"].is_null() {
                "read_volume_first"
            } else {
                "selection_changed"
            }));
        }
        let Some((volume, muted)) = target.state()? else {
            return Ok(unavailable("slider_unavailable"));
        };
        if change.is_null() {
            return Ok(target.result(volume, muted));
        }
        let Some(change) = Update::decode(change) else {
            return Ok(unavailable("slider_unavailable"));
        };
        if !verified_owner(target.hwnd, target.pid) {
            return Ok(unavailable("selection_changed"));
        }
        if !change.matches(volume, muted) {
            match change {
                Update::Volume(wanted) => target
                    .slider
                    .SetValue(wanted * target.slider.CurrentMaximum()? / 100.0)?,
                Update::Mute(_) => {
                    let Ok(invoke) = target
                        .mute
                        .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                    else {
                        return Ok(unavailable("mute_unavailable"));
                    };
                    if !target.mute.CurrentIsEnabled()?.as_bool() {
                        return Ok(unavailable("mute_unavailable"));
                    }
                    invoke.Invoke()?;
                }
            }
        }
        let deadline = Instant::now() + Duration::from_millis(550);
        loop {
            let Some((volume, muted)) = target.state()? else {
                return Ok(unavailable("slider_unavailable"));
            };
            if change.matches(volume, muted) {
                return Ok(target.result(volume, muted));
            }
            if Instant::now() >= deadline {
                return Ok(unavailable("player_did_not_confirm"));
            }
            sleep(Duration::from_millis(25));
        }
    }
}
