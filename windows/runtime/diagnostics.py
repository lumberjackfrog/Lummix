"""Small local diagnostic event log. Only fixed event codes enter the file."""
from datetime import datetime, timezone
import os
from pathlib import Path
import sys
from threading import Lock

from release_identity import VERSION

LIMIT = 64 * 1024
EVENTS = set("""opened closed startup_failed studio_failed wrist_failed spotify_failed
    window_failed update_available update_verified update_network update_invalid
    update_storage update_busy update_launched update_blocked""".split())
_lock = Lock()


def folder():
    return Path(os.environ["LOCALAPPDATA"]) / "LumMix" / "Diagnostics"


def record(code):
    if code not in EVENTS:
        return
    try:
        build = sys.getwindowsversion().build
        stamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        line = f"{stamp} Lummix={VERSION} WinBuild={build} {code}\n".encode("ascii")
        with _lock:
            root = folder()
            root.mkdir(exist_ok=True, parents=True)
            path = root / "lummix.log"
            if path.exists() and path.stat().st_size + len(line) > LIMIT:
                path.replace(root / "lummix.previous.log")
            with path.open("ab") as log:
                log.write(line)
    except OSError:
        pass


def clear():
    with _lock:
        for name in ("lummix.log", "lummix.previous.log"):
            (folder() / name).unlink(missing_ok=True)
