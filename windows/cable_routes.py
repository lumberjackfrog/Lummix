"""Read names of the installed basic signed VB-CABLE pair by identity, including user aliases."""
import threading
import time
import comtypes
from pycaw.pycaw import AudioUtilities

RENDERS=("CABLE Input (VB-Audio Virtual Cable)", "Speakers (VB-Audio Virtual Cable)", "CABLE In 16 Ch (VB-Audio Virtual Cable)")
_lock=threading.Lock()
_cached=None
_checked=0.0


def signed_names(cable_ids):
    recording=cable_ids.get("Capture",[])
    if len(recording)!=1 or not cable_ids.get("Render"):return None
    signed_render={s.casefold() for s in cable_ids["Render"]}
    comtypes.CoInitialize()
    try:
        rend=[d.FriendlyName for d in AudioUtilities.GetAllDevices(0,1) if d.id.casefold() in signed_render]
        rec=[d.FriendlyName for d in AudioUtilities.GetAllDevices(1,1) if d.id.casefold()==recording[0].casefold()]
    finally:
        comtypes.CoUninitialize()
    if len(rec)!=1 or not rec[0] or not rend:return None
    chosen=min(rend,key=lambda s:("16 ch" in s.lower(),RENDERS.index(s) if s in RENDERS else len(RENDERS),s))
    return {"render":chosen,"capture":rec[0]}


def endpoint_names():
    from mic_routes import signed_cables
    global _cached,_checked
    with _lock:
        if _cached is not None and time.monotonic()-_checked<8:
            return _cached
        _cached=signed_names(signed_cables())
        _checked=time.monotonic()
        return _cached
