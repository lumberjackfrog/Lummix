"""Small local diagnostic event log. Only fixed event codes enter the file."""
from datetime import datetime, timezone
from http.client import HTTPException
import json
import os
from pathlib import Path
import sys
from threading import Lock, Thread

from release_identity import VERSION

LIMIT = 64 * 1024
EVENTS = set("""opened closed startup_failed studio_failed wrist_failed spotify_failed
    window_failed update_available update_verified update_network update_invalid
    update_storage update_busy update_launched update_blocked""".split())
_lock = Lock()
_snapshot = Lock()
_generation = 0
CAPTURE = {"off","connecting","unsupported","waiting_spotify","direct_to_cable","unavailable","receiving","waiting_audio","ambiguous"}


def folder():
    return Path(os.environ["LOCALAPPDATA"]) / "LumMix" / "Diagnostics"


def _write_locked(code):
    try:
        build = sys.getwindowsversion().build
        stamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        line = f"{stamp} Lummix={VERSION} WinBuild={build} {code}\n".encode("ascii")
        root = folder()
        root.mkdir(exist_ok=True, parents=True)
        path = root / "lummix.log"
        if path.exists() and path.stat().st_size + len(line) > LIMIT:
            path.replace(root / "lummix.previous.log")
        with path.open("ab") as log:
            log.write(line)
    except OSError:
        pass


def record(code):
    if code in EVENTS:
        with _lock: _write_locked(code)


def audio_summary(state):
    def flag(key):
        value=state.get(key)
        return ("yes" if value else "no") if type(value) is bool else "unknown"
    capture=state.get("spotifyCapture")
    capture=capture if isinstance(capture,str) and capture in CAPTURE else "unknown"
    return f"audio_snapshot mix={flag('routeActive')} cable={flag('cableReady')} spotify={capture}"


def request_audio_snapshot():
    if not _snapshot.acquire(blocking=False): return
    with _lock: generation=_generation
    def read():
        summary="audio_status_unavailable"
        try:
            from lummix_window import ORIGIN, request
            code,_,body=request(41838,"/mix/mic",headers={"Origin":ORIGIN},limit=32769)
            if code==200 and len(body)<=32768:
                try: summary=audio_summary(json.loads(body))
                except (ValueError,AttributeError): pass
        except (OSError, HTTPException):
            pass
        finally:
            with _lock:
                if generation==_generation: _write_locked(summary)
            _snapshot.release()
    Thread(target=read,name="Lummix diagnostic snapshot",daemon=True).start()


def clear():
    global _generation
    with _lock:
        _generation += 1
        for name in ("lummix.log", "lummix.previous.log"):
            (folder() / name).unlink(missing_ok=True)
