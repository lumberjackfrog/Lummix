"""Check whether the player's own render stream already feeds the mixer output."""
import comtypes
from pycaw.api.audiopolicy import IAudioSessionControl2, IAudioSessionManager2
from spotify_native import provider as media_provider


def session_state(device, tree):
    """Return (active, inactive) for this process tree on one render endpoint."""
    manager = device.Activate(IAudioSessionManager2._iid_, comtypes.CLSCTX_ALL, None)
    sessions = manager.QueryInterface(IAudioSessionManager2).GetSessionEnumerator()
    inactive = False
    for i in range(sessions.GetCount()):
        ctl = sessions.GetSession(i).QueryInterface(IAudioSessionControl2)
        if ctl.GetProcessId() in tree:
            state = ctl.GetState()
            if state == 1:
                return True, inactive
            inactive |= state == 0
    return False, inactive


def inspect_routes(devices, signed_ids, tree):
    candidates = [dev for dev in devices if dev.GetId().casefold() in signed_ids]
    if not candidates:
        return None
    idle_on_cable = False
    for dev in candidates:
        active, inactive = session_state(dev, tree)
        if active:
            return True
        idle_on_cable |= inactive
    if idle_on_cable:
        # Windows can retain old sessions after switching outputs. A live stream
        # on headphones clears an idle cable entry without waiting for expiry.
        for dev in devices:
            if dev.GetId().casefold() not in signed_ids and session_state(dev, tree)[0]:
                return False
        return None
    return False


def direct_to_cable(root_pid: int, signed_render_ids) -> bool | None:
    """True means a direct stream, None means the route cannot be confirmed."""
    ids = {s.casefold() for s in signed_render_ids if isinstance(s, str)}
    if not ids or not root_pid:
        return None
    try:
        native = media_provider()
        if native is not None:
            return native.direct_to_cable(root_pid, ids)
    except (OSError, RuntimeError):
        pass
    return None
