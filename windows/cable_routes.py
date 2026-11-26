"""Read names of the installed basic signed VB-CABLE pair by identity, including user aliases."""
import threading
import time
from cable_alias import signed_names

# Older mixer revisions import these names when run by the comparison tools.
RENDERS = ("CABLE Input (VB-Audio Virtual Cable)", "Speakers (VB-Audio Virtual Cable)",
           "CABLE In 16 Ch (VB-Audio Virtual Cable)")

_lock=threading.Lock()
_cached=None
_checked=0.0


def endpoint_names():
    from mic_routes import signed_cables
    global _cached,_checked
    with _lock:
        if _cached is not None and time.monotonic()-_checked<8:
            return _cached
        _cached=signed_names(signed_cables())
        _checked=time.monotonic()
        return _cached
