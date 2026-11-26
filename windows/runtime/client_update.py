"""One user-controlled update at a time, off the audio and window threads."""
from contextlib import contextmanager
import hashlib
from http.client import HTTPException
import json
import os
from pathlib import Path
import threading
import time
from urllib.error import URLError
import pywintypes

import diagnostics
from release_identity import VERSION
import update_feed as feed


def route_idle():
    from lummix_window import ORIGIN, request
    try:
        code, _, body = request(41838,"/mix/gains",headers={"Origin":ORIGIN},limit=8192)
        return code == 200 and json.loads(body).get("routeActive") is False
    except (OSError, ValueError, AttributeError, HTTPException):
        return False


@contextmanager
def locked_installer(path):
    # Deny replacement/deletion between the final hash check and the shell opening the EXE.
    import msvcrt
    import win32con
    import win32file
    handle = win32file.CreateFile(str(path), win32con.GENERIC_READ, win32con.FILE_SHARE_READ,
                                  None, win32con.OPEN_EXISTING, win32con.FILE_ATTRIBUTE_NORMAL, None)
    fd = msvcrt.open_osfhandle(handle.Detach(), os.O_RDONLY | os.O_BINARY)
    with os.fdopen(fd,"rb") as stream:
        yield stream


def launch_installer(release, path, cancelled, idle=route_idle, shell=os.startfile):
    if not idle():
        return False
    feed.plain(path.parent)
    feed.plain(path)
    with locked_installer(path) as file:
        if os.fstat(file.fileno()).st_size != release.size or hashlib.file_digest(file,"sha256").hexdigest() != release.digest:
            raise feed.InvalidUpdate("staged installer changed")
        if cancelled.is_set() or not idle():
            return False
        shell(str(path),"open",f"/LUMMIXWAITPID={os.getpid()}")
    return True


class Updates:
    def __init__(self, root, changed):
        self.directory = Path(root) / "Updates"
        self.changed = changed
        self.lock = threading.Lock()
        self.cancelled = threading.Event()
        self.busy = False
        self.release = None
        self.phase = "idle"
        self.message = ""
        self.last_request = 0
        try:
            feed.plain(self.directory)
            for name in (feed.ASSET,"download.part"):
                (self.directory/name).unlink(missing_ok=True)
        except (OSError,feed.InvalidUpdate):
            diagnostics.record("update_storage")

    def status(self):
        with self.lock:
            return dict(type="lummix-update", phase=self.phase, current=VERSION,
                        version=self.release.version if self.release else None, message=self.message)

    def command(self, command):
        if command=="auto":
            try:
                stamp=self.directory/"last-check"
                meta=stamp.stat()
                # A freshly written NTFS timestamp can lead Python's Windows clock by a tick.
                if meta.st_size<=20 and -1 <= time.time()-meta.st_mtime < 21600:
                    with stamp.open("rb") as stream:
                        if stream.read(21)==VERSION.encode("ascii"):
                            with self.lock: self.phase="current"
                            return
            except OSError:
                pass
            command="check"
        with self.lock:
            if self.busy or self.cancelled.is_set(): return
            if command=="check" and self.phase in ("idle","current","error","available"):
                if self.last_request and time.monotonic()-self.last_request < 3: return
                self.last_request=time.monotonic()
                self.phase="checking"
            elif command=="download" and self.phase=="available":
                self.phase="downloading"
            elif command=="install" and self.phase=="ready":
                self.phase="installing"
            else: return
            self.busy=True
            self.message=""
        self.changed()
        threading.Thread(target=self.work,args=(command,),name="Lummix update",daemon=True).start()

    def work(self, command):
        phase, message, event = "error", "", None
        selected=self.release
        try:
            if command=="check":
                selected=feed.latest(VERSION)
                phase="available" if selected else "current"
                if selected:
                    event="update_available"
                else:
                    try:
                        feed.plain(self.directory)
                        self.directory.mkdir(exist_ok=True,parents=True)
                        stamp=self.directory/"last-check"
                        stamp.unlink(missing_ok=True)
                        with stamp.open("xb") as stream: stream.write(VERSION.encode("ascii"))
                    except OSError:
                        pass
            elif command=="download":
                feed.download(selected,self.directory,self.cancelled)
                phase,event="ready","update_verified"
            elif launch_installer(selected,self.directory/feed.ASSET,self.cancelled):
                phase,event="launched","update_launched"
            else:
                phase,event,message="ready","update_busy","Stop your mix before installing. If the mixer is still starting, try again in a moment."
        except feed.InvalidUpdate:
            event,message="update_invalid","Release verification failed. Check again when the publisher has corrected the download."
        except (URLError,TimeoutError,ConnectionError,HTTPException):
            event,message="update_network","Couldn’t reach GitHub. Try again when you’re online."
        except (OSError,pywintypes.error):
            if command=="install":
                phase,event,message="ready","update_blocked","Windows could not open the installer. Check Windows Security or try again."
            else:
                event,message="update_storage","Couldn’t save the update. Check your free disk space and try again."
        except (TypeError, ValueError, KeyError, AttributeError):
            event,message="update_invalid","The release information could not be verified."
        finally:
            with self.lock:
                self.release,self.phase,self.message,self.busy=selected,phase,message,False
            if not self.cancelled.is_set():
                if event: diagnostics.record(event)
                self.changed()
