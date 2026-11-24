"""Use the transport group in a verified Spotify window when Windows media sessions are unavailable."""
from dataclasses import dataclass

from spotify_volume_http import VolumeError, controls, spotify_windows


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
        return dispatch(buttons, action)
    except (OSError, comtypes.COMError) as error:
        raise VolumeError("no_player_controls") from error
    finally:
        comtypes.CoUninitialize()
