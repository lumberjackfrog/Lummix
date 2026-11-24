"""Manage the two Lummix XSOverlay entry points without replacing vendor or user content."""
import ctypes
import hashlib
import json
import os
import re
import stat
import tempfile
import threading
from pathlib import Path


NAME = "Lummix"
APP_URL = "http://127.0.0.1:41839/wrist"
RECEIPT = ".lummix-install.json"
BEGIN = b"<!-- Lummix wrist add-on BEGIN -->"
END = b"<!-- Lummix wrist add-on END -->"
FILES = ("app.json", "icon.png")
LOCK = threading.RLock()


class Conflict(Exception):
    pass


def digest(data):
    return hashlib.sha256(data).hexdigest()


def result(state, message, *, removable=False):
    return dict(name=NAME, state=state, message=message, canInstall=state=="installable",
                canUpdate=state=="update", canRemove=removable)


def regular(path, directory=False):
    """Never traverse symlinks or Windows junctions in managed paths."""
    try:
        info=path.lstat()
    except FileNotFoundError:
        return False
    if (stat.S_ISLNK(info.st_mode) or getattr(info,"st_file_attributes",0) & 0x400
            or not (stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode))):
        raise Conflict("An XSOverlay add-on path is redirected or has an unexpected file type.")
    return True


def read(path):
    if not regular(path):
        return None
    if path.stat().st_size > 4*1024*1024:
        raise Conflict("An existing add-on file is too large to identify safely.")
    return path.read_bytes()


def writable_directory(path, create_child=False):
    if os.name!="nt":
        return os.access(path,os.W_OK|os.X_OK)
    from ctypes import wintypes
    kernel=ctypes.WinDLL("kernel32",use_last_error=True)
    create=kernel.CreateFileW
    create.argtypes=(wintypes.LPCWSTR,wintypes.DWORD,wintypes.DWORD,wintypes.LPVOID,
                     wintypes.DWORD,wintypes.DWORD,wintypes.HANDLE)
    create.restype=wintypes.HANDLE
    # Request the right to create an ordinary file or child directory. OPEN_EXISTING never writes.
    handle=create(str(path),4 if create_child else 2,7,None,3,0x02000000,None)
    if handle==ctypes.c_void_p(-1).value:
        return False
    kernel.CloseHandle.argtypes=(wintypes.HANDLE,)
    kernel.CloseHandle(handle)
    return True


def split_wrist(wrist):
    if wrist.count(BEGIN)!=wrist.count(END) or wrist.count(BEGIN)>1:
        raise Conflict("The Lummix markers in Wrist.html are incomplete or duplicated.")
    if BEGIN not in wrist:
        return wrist,None
    start=wrist.index(BEGIN)
    finish=wrist.index(END)+len(END)
    if finish<=start:
        raise Conflict("The Lummix wrist markers are out of order.")
    return wrist[:start]+wrist[finish:],wrist[start:finish]


def write_atomic(path, data):
    if data is None:
        path.unlink()
        return
    fd,name=tempfile.mkstemp(prefix=".lummix-",dir=path.parent)
    try:
        with os.fdopen(fd,"wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        if path.exists():
            os.chmod(name,stat.S_IMODE(path.stat().st_mode))
        os.replace(name,path)
    finally:
        Path(name).unlink(missing_ok=True)


class AddonInstall:
    def __init__(self, discover, data_dir):
        self.discover=discover
        self.data_dir=Path(data_dir)

    def assets(self):
        script=(self.data_dir/"xsoverlay-live.js").read_bytes()
        icon=(self.data_dir/"xsoverlay-icon.png").read_bytes()
        if not script.strip() or re.search(br"</script",script,re.I) or not icon.startswith(b"\x89PNG\r\n\x1a\n"):
            raise ValueError("Invalid bundled payload")
        script.decode("utf-8")
        block=BEGIN+b'\n<script>\n'+script+b'\n</script>\n'+END
        app={"author":"Lummix","name":NAME,"version":"v1.0","description":"Lummix microphone and music controls.",
             "url":APP_URL,"resWidth":720,"resHeight":480,"iconColor":[0.68,0.52,0.78],
             "defaultScale":0.2,"canOpenMultiple":False,"canBeCurved":False}
        files={"app.json":(json.dumps(app,indent=2)+"\n").encode(),"icon.png":icon}
        receipt={"owner":"Lummix","format":1,"files":{k:digest(v) for k,v in files.items()},"block":digest(block)}
        files[RECEIPT]=(json.dumps(receipt,sort_keys=True)+"\n").encode()
        return files,block

    def snapshot(self):
        raw_root=self.discover()
        if not raw_root:
            return result("missing","XSOverlay was not found in a Steam library."),None
        root=Path(raw_root)
        if root.name.casefold()!="applications" or not regular(root,directory=True):
            return result("unsupported","The discovered XSOverlay installation has no compatible Applications directory."),None
        for folder in (root/"_UI",root/"_UI"/"Default"):
            if not regular(folder,directory=True):
                return result("unsupported","This XSOverlay installation has no supported wrist layout."),None
        wrist_path=root/"_UI"/"Default"/"Wrist.html"
        wrist=read(wrist_path)
        if wrist is None:
            return result("unsupported","XSOverlay's wrist page is missing. Verify its files in Steam."),None
        clean,installed_block=split_wrist(wrist)
        supported=(clean.lower().count(b"</body>")==1 and
                   all(key in clean for key in (b'uiContainer',b'./_Shared/js/uiComponents.js',b'./_Shared/js/toolbar.js')))
        files,block=self.assets()
        appdir=root/NAME
        exists=regular(appdir,directory=True)
        before={n:read(appdir/n) for n in (*FILES,RECEIPT)} if exists else {n:None for n in (*FILES,RECEIPT)}
        old=before[RECEIPT]
        if old is not None:
            try:
                proof=json.loads(old)
                valid=(proof["owner"]==NAME and proof["format"]==1 and set(proof["files"])==set(FILES)
                       and all(re.fullmatch("[0-9a-f]{64}",v) for v in [proof["block"],*proof["files"].values()]))
            except (TypeError,ValueError,KeyError,AttributeError):
                valid=False
            if not valid:
                raise Conflict("The Lummix ownership receipt is unrecognized. Existing files were preserved.")
            for n in FILES:
                if before[n] is not None and digest(before[n])!=proof["files"][n]:
                    raise Conflict(f"The existing Lummix {n} was edited. Your copy was preserved.")
            if installed_block is not None and digest(installed_block)!=proof["block"]:
                raise Conflict("The installed Lummix wrist code was edited. Your copy was preserved.")
        elif exists or (installed_block is not None and installed_block!=block):
            raise Conflict("An unrecognized Lummix folder or wrist block already exists. Your files were preserved.")
        installed=old is not None or installed_block is not None
        bundle=(appdir,wrist_path,wrist,clean,before,files,block)
        if not supported:
            return result("unsupported","This XSOverlay wrist layout is not supported.",removable=installed),bundle
        writable=writable_directory(wrist_path.parent) and writable_directory(appdir if exists else root,create_child=not exists)
        if installed and before==files and installed_block==block:
            return result("ready","Installed in XSOverlay." if writable else "Installed. This Steam library is read-only.",
                          removable=writable),bundle
        if not writable:
            return result("blocked","Windows denied write access to the XSOverlay Steam library."),None
        if installed:
            return result("update","The add-on has an update or a missing file to restore.",removable=True),bundle
        return result("installable","XSOverlay is ready for the wrist add-on."),bundle

    def inspect(self):
        try:
            return self.snapshot()
        except Conflict as exc:
            return result("conflict",str(exc)),None
        except PermissionError:
            return result("blocked","Windows denied access to the XSOverlay Steam library."),None
        except (ValueError,OSError):
            return result("unsupported","The Lummix add-on files or XSOverlay installation could not be read."),None

    def status(self):
        with LOCK:
            return self.inspect()[0]

    def change(self, action):
        if action not in ("install","remove"):
            raise ValueError("Unknown add-on action")
        with LOCK:
            state,bundle=self.inspect()
            if not (state["canRemove"] if action=="remove" else state["canInstall"] or state["canUpdate"]):
                return state
            appdir,wrist_path,wrist,clean,before,files,block=bundle
            installing=action=="install"
            after=clean
            if installing:
                index=clean.lower().index(b"</body>")
                after=clean[:index]+block+clean[index:]
            app_changes=[(appdir/n,before[n],files[n] if installing else None)
                         for n in ((RECEIPT,*FILES) if installing else (*FILES,RECEIPT))]
            wrist_change=(wrist_path,wrist,after)
            changes=app_changes+[wrist_change] if installing else [wrist_change]+app_changes
            written=[]
            created=False
            try:
                if installing and not appdir.exists():
                    appdir.mkdir()
                    created=True
                for path,previous,next_value in changes:
                    if previous==next_value:continue
                    if read(path)!=previous:
                        raise Conflict("An XSOverlay file changed during the operation. Check again.")
                    write_atomic(path,next_value)
                    written.append((path,previous,next_value))
            except (OSError,Conflict) as exc:
                restored=True
                for path,previous,ours in reversed(written):
                    try:
                        if read(path)!=ours:restored=False
                        else:write_atomic(path,previous)
                    except (OSError,Conflict):
                        restored=False
                if created:
                    try:appdir.rmdir()
                    except OSError:restored=False
                if not restored:
                    return result("conflict","The change stopped during rollback. Check your XSOverlay files before retrying.")
                return result("conflict" if isinstance(exc,Conflict) else "blocked",
                              "The change was rolled back. Close XSOverlay and check Steam library write access.")
            if not installing and appdir.exists():
                try:appdir.rmdir()
                except OSError:pass  # Other files belong to the user.
            return self.status()
