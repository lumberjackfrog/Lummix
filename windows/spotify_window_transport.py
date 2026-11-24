"""Use the transport group in a verified Spotify window when Windows media sessions are unavailable."""
from dataclasses import dataclass

from spotify_volume_http import VolumeError, controls, spotify_windows
from spotify_artwork import spotify_item


@dataclass(frozen=True)
class WindowPlayer:
    edition: str
    identity: tuple  # Window handle, process id and process creation time.


LABELS = {
    "play": "play", "pause": "pause", "previous": "previous", "next": "next",
    "reproducir": "play", "pausar": "pause", "anterior": "previous", "siguiente": "next",
}
GROUPS = ("Player controls", "Controles del reproductor")


def dispatch(buttons, action):
    playing = "pause" in buttons
    if playing == ("play" in buttons):
        raise VolumeError("no_player_controls")
    if action is not None:
        if action in buttons:
            buttons[action].Invoke()
        elif not ((action == "play" and playing) or (action == "pause" and not playing)):
            raise VolumeError("control_disabled")
    return dict(playing=playing, canPlay=not playing, canPause=playing,
                canPrevious="previous" in buttons, canNext="next" in buttons)


def now_playing(ui, uia, bar):
    if not bar or bar.CurrentAriaRole != "complementary":
        return {}
    regions = bar.FindAll(uia.TreeScope_Children, ui.CreatePropertyCondition(uia.UIA_AriaRolePropertyId, "region"))
    if regions.Length != 1:
        return {}
    links = controls(ui, uia, regions.GetElement(0), uia.UIA_HyperlinkControlTypeId)
    albums, artists = [], []
    for link in links:
        value = link.GetCurrentPropertyValue(uia.UIA_ValueValuePropertyId)
        item = spotify_item(value)
        if item:
            albums.append((item, link.CurrentName.strip()))
        elif isinstance(value, str) and value.startswith(("https://xpui.app.spotify.com/artist/", "https://open.spotify.com/artist/")):
            artists.append(link.CurrentName.strip())
    if len(albums) != 1:
        return {}
    item, title = albums[0]
    return {"itemUrl": item, "title": title[:256], "artist": ", ".join(artists)[:256]}


def artwork_bar(ui, uia, root):
    # ARIA roles are language independent. The player bar has a direct track region and seek slider.
    bars = root.FindAll(uia.TreeScope_Descendants, ui.CreatePropertyCondition(uia.UIA_AriaRolePropertyId, "complementary"))
    matches = []
    for index in range(bars.Length):
        bar = bars.GetElement(index)
        groups = controls(ui, uia, bar, uia.UIA_GroupControlTypeId, uia.TreeScope_Children)
        if any(controls(ui, uia, group, uia.UIA_SliderControlTypeId) for group in groups):
            metadata = now_playing(ui, uia, bar)
            if metadata:
                matches.append(metadata)
    return matches[0] if len(matches) == 1 else {}


def transport(player, action=None):
    import comtypes
    import comtypes.client
    comtypes.CoInitialize()
    try:
        from comtypes.gen import UIAutomationClient as uia
        if spotify_windows(player.edition) != [player.identity]:
            raise VolumeError("selection_changed")
        ui = comtypes.client.CreateObject(uia.CUIAutomation, interface=uia.IUIAutomation)
        root = ui.ElementFromHandle(player.identity[0])
        names = ui.CreateOrCondition(*(ui.CreatePropertyCondition(uia.UIA_NamePropertyId, name) for name in GROUPS))
        condition = ui.CreateAndCondition(names, ui.CreatePropertyCondition(uia.UIA_ControlTypePropertyId,
                                                                           uia.UIA_GroupControlTypeId))
        groups = root.FindAll(uia.TreeScope_Descendants, condition)
        if groups.Length != 1 and action is None:
            return artwork_bar(ui, uia, root)
        if groups.Length != 1:
            raise VolumeError("no_player_controls")
        buttons = {}
        for button in controls(ui, uia, groups.GetElement(0), uia.UIA_ButtonControlTypeId, uia.TreeScope_Children):
            key = LABELS.get(button.CurrentName.strip().casefold())
            if not key or not button.CurrentIsEnabled:
                continue
            if key in buttons:
                raise VolumeError("ambiguous_player_controls")
            pattern = button.GetCurrentPattern(uia.UIA_InvokePatternId)
            if pattern:
                buttons[key] = pattern.QueryInterface(uia.IUIAutomationInvokePattern)
        try:
            result = dispatch(buttons, action)
        except VolumeError:
            if action is not None:
                raise
            result = {}
        if action is None:
            try:
                result.update(now_playing(ui, uia, ui.ControlViewWalker.GetParentElement(groups.GetElement(0))))
            except (OSError, comtypes.COMError):
                pass
        return result
    except (OSError, comtypes.COMError) as error:
        raise VolumeError("no_player_controls") from error
    finally:
        comtypes.CoUninitialize()
