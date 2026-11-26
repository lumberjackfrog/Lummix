"""Manage the two Lummix XSOverlay entry points without replacing vendor or user content."""
import ctypes
from functools import lru_cache
import hashlib
import json
import os
import re
import stat
import sys
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


class Layout(ctypes.Structure):
    _fields_ = [("start", ctypes.c_size_t), ("end", ctypes.c_size_t),
                ("insert", ctypes.c_size_t), ("compatible", ctypes.c_uint)]


FileOperation = ctypes.CFUNCTYPE(ctypes.c_uint, ctypes.c_uint, ctypes.c_uint)


@lru_cache(maxsize=1)
def planner():
    if getattr(sys, "frozen", False):
        from native_host import library
    else:
        from runtime.native_host import library
    dll = library()
    layout = dll.lummix_xsoverlay_layout
    layout.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(Layout)]
    layout.restype = ctypes.c_uint
    ownership = dll.lummix_xsoverlay_ownership
    ownership.argtypes = [ctypes.c_char_p, ctypes.c_size_t]
    ownership.restype = ctypes.c_uint
    disposition = dll.lummix_xsoverlay_disposition
    disposition.argtypes = [ctypes.c_bool, ctypes.c_bool, ctypes.c_uint]
    disposition.restype = ctypes.c_uint
    transaction = dll.lummix_xsoverlay_transaction
    transaction.argtypes = [ctypes.c_bool, FileOperation]
    transaction.restype = ctypes.c_uint
    return layout, ownership, disposition, transaction


def inspect_layout(wrist):
    layout = Layout()
    status = planner()[0](wrist, len(wrist), ctypes.byref(layout))
    if status == 1:
        raise Conflict("The Lummix markers in Wrist.html are incomplete or duplicated.")
    if status == 2:
        raise Conflict("The Lummix wrist markers are out of order.")
    if status:
        raise ValueError("The wrist layout could not be read.")
    clean = wrist[:layout.start] + wrist[layout.end:]
    block = wrist[layout.start:layout.end] if layout.end else None
    return layout, clean, block


def inspect_ownership(before, new, installed, block, directory):
    receipt = before[RECEIPT]
    try:
        proof = json.loads(receipt) if receipt is not None else None
    except (ValueError, TypeError):
        proof = None
    facts = dict(receipt=proof, hasReceipt=receipt is not None, directory=directory,
                 manifest=digest(before["app.json"]) if before["app.json"] is not None else None,
                 icon=digest(before["icon.png"]) if before["icon.png"] is not None else None,
                 block=digest(installed) if installed is not None else None,
                 expectedBlock=digest(block), sameFiles=before == new)
    try:
        data = json.dumps(facts, separators=(",", ":"), allow_nan=False).encode()
    except ValueError:
        facts["receipt"] = None
        data = json.dumps(facts, separators=(",", ":")).encode()
    status = planner()[1](data, len(data))
    errors = {
        3: "The Lummix ownership receipt is unrecognized. Existing files were preserved.",
        4: "The existing Lummix app.json was edited. Your copy was preserved.",
        5: "The existing Lummix icon.png was edited. Your copy was preserved.",
        6: "The installed Lummix wrist code was edited. Your copy was preserved.",
        7: "An unrecognized Lummix folder or wrist block already exists. Your files were preserved.",
    }
    if status in errors:
        raise Conflict(errors[status])
    if status not in (0, 1, 2):
        raise ValueError("The wrist ownership could not be read.")
    return status


def installation_status(layout, writable, ownership):
    status = planner()[2](bool(layout.compatible), bool(writable), ownership)
    states = {
        0: ("installable", "XSOverlay is ready for the wrist add-on.", False),
        1: ("ready", "Installed in XSOverlay.", True),
        2: ("ready", "Installed. This Steam library is read-only.", False),
        3: ("update", "The add-on has an update or a missing file to restore.", True),
        4: ("unsupported", "This XSOverlay wrist layout is not supported.", False),
        5: ("unsupported", "This XSOverlay wrist layout is not supported.", True),
        6: ("blocked", "Windows denied write access to the XSOverlay Steam library.", False),
    }
    name, message, removable = states[status]
    return result(name, message, removable=removable)


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
        layout,clean,installed_block=inspect_layout(wrist)
        files,block=self.assets()
        appdir=root/NAME
        exists=regular(appdir,directory=True)
        before={n:read(appdir/n) for n in (*FILES,RECEIPT)} if exists else {n:None for n in (*FILES,RECEIPT)}
        ownership=inspect_ownership(before,files,installed_block,block,exists)
        writable=layout.compatible and writable_directory(wrist_path.parent) and writable_directory(
            appdir if exists else root,create_child=not exists)
        status=installation_status(layout,writable,ownership)
        bundle=(appdir,wrist_path,wrist,clean,before,files,block,layout.insert)
        return status,bundle if status["state"]!="blocked" else None

    def inspect(self):
        try:
            return self.snapshot()
        except Conflict as exc:
            return result("conflict",str(exc)),None
        except PermissionError:
            return result("blocked","Windows denied access to the XSOverlay Steam library."),None
        except (ValueError,OSError,AttributeError):
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
            appdir,wrist_path,wrist,clean,before,files,block,index=bundle
            installing=action=="install"
            after=clean
            if installing:
                after=clean[:index]+block+clean[index:]
            changes=[(appdir/name,before[name],files[name] if installing else None)
                     for name in (RECEIPT,*FILES)]
            changes.append((wrist_path,wrist,after))
            unexpected=[]

            @FileOperation
            def file_operation(phase,slot):
                try:
                    if phase==0:  # Prepare the managed directory for install.
                        if appdir.exists():
                            return 0
                        appdir.mkdir()
                        return 1
                    if phase==3:
                        appdir.rmdir()
                        return 1
                    if phase==4:  # Users can keep other files in this directory.
                        try:
                            appdir.rmdir()
                        except OSError:
                            pass
                        return 1
                    path,previous,next_value=changes[slot]
                    if phase==1 and previous==next_value:
                        return 0
                    expected,replacement=(previous,next_value) if phase==1 else (next_value,previous)
                    if read(path)!=expected:
                        return 2
                    write_atomic(path,replacement)
                    return 1
                except Conflict:
                    return 2
                except OSError:
                    return 3
                except BaseException as error:
                    # Exceptions cannot cross a ctypes callback. Let Rust
                    # restore already-written files before raising in Python.
                    unexpected.append(error)
                    return 3

            outcome=planner()[3](installing,file_operation)
            if unexpected:
                raise unexpected[0]
            if outcome==4:
                return result("conflict","The change stopped during rollback. Check your XSOverlay files before retrying.")
            if outcome!=1:
                return result("conflict" if outcome==2 else "blocked",
                              "The change was rolled back. Close XSOverlay and check Steam library write access.")
            return self.status()
