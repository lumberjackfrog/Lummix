use serde_json::{json, Value};
use std::collections::HashMap;
use windows::{
    core::{Result, BSTR, VARIANT},
    Win32::{
        Foundation::{BOOL, HWND, LPARAM},
        System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
        UI::{
            Accessibility::{
                CUIAutomation, IUIAutomation, IUIAutomationCondition, IUIAutomationElement,
                IUIAutomationInvokePattern, TreeScope, TreeScope_Children, TreeScope_Descendants,
                UIA_AriaRolePropertyId, UIA_ButtonControlTypeId, UIA_ControlTypePropertyId,
                UIA_GroupControlTypeId, UIA_HyperlinkControlTypeId, UIA_InvokePatternId,
                UIA_NamePropertyId, UIA_SliderControlTypeId, UIA_ValueValuePropertyId,
                UIA_CONTROLTYPE_ID,
            },
            WindowsAndMessaging::{
                EnumWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId,
                IsWindowVisible,
            },
        },
    },
};

fn result_error(reason: &str) -> Value {
    json!({"reason": reason})
}

unsafe fn find(
    root: &IUIAutomationElement,
    scope: TreeScope,
    condition: &IUIAutomationCondition,
) -> Result<Vec<IUIAutomationElement>> {
    let matches = root.FindAll(scope, condition)?;
    (0..matches.Length()?)
        .map(|index| matches.GetElement(index))
        .collect()
}

pub(super) unsafe fn controls(
    ui: &IUIAutomation,
    root: &IUIAutomationElement,
    scope: TreeScope,
    kind: UIA_CONTROLTYPE_ID,
) -> Result<Vec<IUIAutomationElement>> {
    let condition =
        ui.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(kind.0))?;
    find(root, scope, &condition)
}

fn item_url(value: &str) -> Option<String> {
    let path = value
        .strip_prefix("https://xpui.app.spotify.com/")
        .or_else(|| value.strip_prefix("https://open.spotify.com/"))?;
    let path = match path.split_once('?') {
        Some((path, query)) if !query.contains(|ch: char| ch == '#' || ch.is_whitespace()) => path,
        Some(_) => return None,
        None => path,
    };
    let (kind, id) = path.split_once('/')?;
    if matches!(kind, "album" | "track" | "episode")
        && id.len() == 22
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Some(format!("https://open.spotify.com/{path}"));
    }
    None
}

unsafe fn role(ui: &IUIAutomation, value: &str) -> Result<IUIAutomationCondition> {
    ui.CreatePropertyCondition(UIA_AriaRolePropertyId, &VARIANT::from(BSTR::from(value)))
}

unsafe fn now_playing(ui: &IUIAutomation, bar: &IUIAutomationElement) -> Result<Option<Value>> {
    if bar.CurrentAriaRole()?.to_string() != "complementary" {
        return Ok(None);
    }
    let regions = find(bar, TreeScope_Children, &role(ui, "region")?)?;
    let [region] = regions.as_slice() else {
        return Ok(None);
    };
    let mut item = None;
    let mut artists = Vec::new();
    for link in controls(
        ui,
        region,
        TreeScope_Descendants,
        UIA_HyperlinkControlTypeId,
    )? {
        let value = link.GetCurrentPropertyValue(UIA_ValueValuePropertyId)?;
        let Ok(value) = BSTR::try_from(&value) else {
            continue;
        };
        let value = value.to_string();
        if let Some(url) = item_url(&value) {
            if item.is_some() {
                return Ok(None);
            }
            let title = link.CurrentName()?.to_string();
            item = Some((url, title.trim().chars().take(256).collect::<String>()));
        } else if value.starts_with("https://xpui.app.spotify.com/artist/")
            || value.starts_with("https://open.spotify.com/artist/")
        {
            artists.push(link.CurrentName()?.to_string().trim().to_owned());
        }
    }
    Ok(item.map(|(url, title)| {
        json!({"itemUrl": url, "title": title,
        "artist": artists.join(", ").chars().take(256).collect::<String>()})
    }))
}

unsafe fn artwork_bar(ui: &IUIAutomation, root: &IUIAutomationElement) -> Result<Value> {
    let mut match_metadata = None;
    for bar in find(root, TreeScope_Descendants, &role(ui, "complementary")?)? {
        for group in controls(ui, &bar, TreeScope_Children, UIA_GroupControlTypeId)? {
            if controls(ui, &group, TreeScope_Descendants, UIA_SliderControlTypeId)?.is_empty() {
                continue;
            }
            if let Some(metadata) = now_playing(ui, &bar)? {
                if match_metadata.is_some() {
                    return Ok(json!({}));
                }
                match_metadata = Some(metadata);
            }
            break;
        }
    }
    Ok(match_metadata.unwrap_or_else(|| json!({})))
}

fn button_action(name: &str) -> Option<u32> {
    match name.trim().to_lowercase().as_str() {
        "play" | "reproducir" => Some(1),
        "pause" | "pausar" => Some(2),
        "previous" | "anterior" => Some(3),
        "next" | "siguiente" => Some(4),
        _ => None,
    }
}

unsafe fn player_groups(
    ui: &IUIAutomation,
    root: &IUIAutomationElement,
) -> Result<Vec<IUIAutomationElement>> {
    let named =
        |name| ui.CreatePropertyCondition(UIA_NamePropertyId, &VARIANT::from(BSTR::from(name)));
    let name = ui.CreateOrCondition(
        &named("Player controls")?,
        &named("Controles del reproductor")?,
    )?;
    let group = ui.CreatePropertyCondition(
        UIA_ControlTypePropertyId,
        &VARIANT::from(UIA_GroupControlTypeId.0),
    )?;
    find(
        root,
        TreeScope_Descendants,
        &ui.CreateAndCondition(&name, &group)?,
    )
}

pub(super) unsafe fn verified_owner(hwnd: HWND, pid: u32) -> bool {
    let mut actual = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut actual)) != 0 && actual == pid
}

pub(super) fn visible_windows() -> Result<Value> {
    unsafe extern "system" fn visit(hwnd: HWND, rows: LPARAM) -> BOOL {
        let mut kind = [0u16; 64];
        let mut pid = 0;
        if IsWindowVisible(hwnd).as_bool() {
            let length = GetClassNameW(hwnd, &mut kind).max(0) as usize;
            if String::from_utf16_lossy(&kind[..length]) == "Chrome_WidgetWin_1"
                && GetWindowThreadProcessId(hwnd, Some(&mut pid)) != 0
            {
                let rows = &mut *(rows.0 as *mut Vec<(u64, u32)>);
                rows.push((hwnd.0 as usize as u64, pid));
            }
        }
        BOOL(1)
    }
    let mut rows = Vec::<(u64, u32)>::new();
    unsafe { EnumWindows(Some(visit), LPARAM((&mut rows as *mut _) as isize))? };
    Ok(json!(rows))
}

pub(super) fn window_title(handle: u64, pid: u32) -> Value {
    let hwnd = HWND(handle as usize as *mut _);
    let mut text = [0u16; 1024];
    unsafe {
        if verified_owner(hwnd, pid) {
            let length = GetWindowTextW(hwnd, &mut text).max(0) as usize;
            return json!(String::from_utf16_lossy(&text[..length]));
        }
    }
    json!("")
}

pub fn transport(handle: u64, pid: u32, action: u32) -> Result<Value> {
    let hwnd = HWND(handle as usize as *mut _);
    unsafe {
        if !verified_owner(hwnd, pid) {
            return Ok(result_error("selection_changed"));
        }
        let ui: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
        let root = ui.ElementFromHandle(hwnd)?;
        let groups = player_groups(&ui, &root)?;
        let [group] = groups.as_slice() else {
            return if action == 0 {
                artwork_bar(&ui, &root)
            } else {
                Ok(result_error("no_player_controls"))
            };
        };
        let mut buttons = HashMap::new();
        for button in controls(&ui, group, TreeScope_Children, UIA_ButtonControlTypeId)? {
            let Some(command) = button_action(&button.CurrentName()?.to_string()) else {
                continue;
            };
            if !button.CurrentIsEnabled()?.as_bool() {
                continue;
            }
            if buttons.contains_key(&command) {
                return Ok(result_error("ambiguous_player_controls"));
            }
            if let Ok(invoke) =
                button.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
            {
                buttons.insert(command, invoke);
            }
        }
        let playing = buttons.contains_key(&2);
        let mut result = if playing == buttons.contains_key(&1) {
            if action != 0 {
                return Ok(result_error("no_player_controls"));
            }
            json!({})
        } else {
            json!({"playing": playing, "canPlay": !playing, "canPause": playing,
                "canPrevious": buttons.contains_key(&3), "canNext": buttons.contains_key(&4)})
        };
        if action != 0 {
            if let Some(button) = buttons.get(&action) {
                if !verified_owner(hwnd, pid) {
                    return Ok(result_error("selection_changed"));
                }
                button.Invoke()?;
            } else if !((action == 1 && playing) || (action == 2 && !playing)) {
                return Ok(result_error("control_disabled"));
            }
        } else if let Ok(Some(metadata)) = ui
            .ControlViewWalker()
            .and_then(|walker| walker.GetParentElement(group))
            .and_then(|bar| now_playing(&ui, &bar))
        {
            result
                .as_object_mut()
                .unwrap()
                .extend(metadata.as_object().unwrap().clone());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_exact_spotify_item_links() {
        let link = "https://xpui.app.spotify.com/track/2N6k0xq9jTYO1234567890";
        let expected = "https://open.spotify.com/track/2N6k0xq9jTYO1234567890";
        assert_eq!(
            item_url(&format!("{link}?si=123")).as_deref(),
            Some(expected)
        );
        for suffix in ["/related", "#section", "?a=#anchor", "?a=two words"] {
            assert_eq!(item_url(&format!("{link}{suffix}")), None);
        }
        assert_eq!(item_url(&link.replace("xpui.app", "attacker")), None);
    }
}
