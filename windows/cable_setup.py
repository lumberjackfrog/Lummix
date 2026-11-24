"""Local, explicit user-initiated setup of the original base VB-CABLE for Windows."""
import argparse
from contextlib import contextmanager, ExitStack
import ctypes
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import tempfile
import threading
import time
from urllib import request
import zipfile

ROOT = Path(__file__).resolve().parent
URL = "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip"
SHA = "bc44e8974191a5a2427d6c979e2dab08dbb09925f3f435dfa2f0ba3e45b6eb90"
SIZE = 1324680
BUSY = {"checking","preparing","awaiting-windows"}
MESSAGES = {
    "ready":"VB-Audio’s signed VB-CABLE playback and microphone endpoints are ready.",
    "missing":"VB-CABLE is available to set up on this PC.",
    "partial":"An existing VB-CABLE installation was found without a verified active pair. Check Windows Sound or restart Windows.",
    "unavailable":"Windows could not verify the audio driver and both endpoints.",
    "preparing":"Preparing and verifying the original VB-Audio package.",
    "awaiting-windows":"Complete the Windows approval prompt if shown. Windows is setting up VB-CABLE.",
    "cancelled":"Windows approval was declined.",
    "failed":"Setup could not be verified. No additional installation was attempted.",
    "restart-needed":"Setup finished. Restart Windows, then reopen Lum Mix to check the endpoints.",
}


def shape(state):
    return {"state":state,"ready":state=="ready","restartMayBeNeeded":state in ("partial","restart-needed"),
            "message":MESSAGES[state]}


def powershell(resource, folder=None, mode=None, wait=15):
    binary=Path(os.environ["SystemRoot"])/"System32/WindowsPowerShell/v1.0/powershell.exe"
    script=(ROOT/resource).read_text(encoding="utf-8")
    env=os.environ.copy()
    env["PSModulePath"]=str(binary.parent/"Modules")
    if folder is not None:
        env.update(LUM_CABLE_FOLDER=str(folder),LUM_CABLE_ACTION=mode)
        script="& {\n"+script+"\n} -Folder $env:LUM_CABLE_FOLDER -Mode $env:LUM_CABLE_ACTION"
    return subprocess.run([str(binary),"-NoLogo","-NoProfile","-NonInteractive","-Command",script],
                          capture_output=True,text=True,env=env,timeout=wait,creationflags=subprocess.CREATE_NO_WINDOW)


def inspect_pair():
    try:
        result=powershell("cable_setup_status.ps1")
        if result.returncode:
            return shape("unavailable")
        found=json.loads(result.stdout)
        if found["state"] not in ("ready","partial","missing"):
            return shape("unavailable")
        if found["state"]=="ready" and not all(found.get(k) is True for k in ("signed","capture","render")):
            return shape("partial")
        info=shape(found["state"])
        if "endpointIds" in found:
            from cable_alias import labels
            info.update(labels(found["endpointIds"]))
        return info
    except (OSError,ValueError,KeyError,subprocess.SubprocessError):
        return shape("unavailable")


class Direct(request.HTTPRedirectHandler):
    def redirect_request(self,*args):
        return None


def download():
    with request.build_opener(Direct()).open(URL,timeout=20) as response:
        if response.status != 200 or response.geturl()!=URL:
            raise ValueError("Unexpected vendor response")
        return response.read(SIZE+1)


def lock_binary(path):
    import msvcrt
    from ctypes import wintypes
    open_file=ctypes.WinDLL("kernel32",use_last_error=True).CreateFileW
    open_file.argtypes=[wintypes.LPCWSTR,wintypes.DWORD,wintypes.DWORD,ctypes.c_void_p,wintypes.DWORD,wintypes.DWORD,wintypes.HANDLE]
    open_file.restype=wintypes.HANDLE
    handle=open_file(str(path),0x80000000,1,None,3,0x80,None)  # GENERIC_READ, FILE_SHARE_READ only
    if handle==ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    return os.fdopen(msvcrt.open_osfhandle(handle,os.O_RDONLY|os.O_BINARY),"rb")


@contextmanager
def package(fetch=download):
    if os.name!="nt" or platform.machine().upper() not in ("AMD64","X86_64","ARM64"):
        raise ValueError("This package launcher supports 64-bit Windows.")
    raw=fetch()
    if len(raw)!=SIZE or hashlib.sha256(raw).hexdigest()!=SHA:
        raise ValueError("The VB-Audio archive does not match the reviewed package.")
    with tempfile.TemporaryDirectory(prefix="LumMix-VBCABLE-") as temp, ExitStack() as guard, zipfile.ZipFile(io.BytesIO(raw)) as archive:
        folder=Path(temp)
        if sum(x.file_size for x in archive.infolist())>10_000_000:
            raise ValueError("Unexpected package size")
        names=set()
        for entry in archive.infolist():
            name=entry.filename
            if not re.fullmatch(r"[A-Za-z0-9_]+\.(exe|inf|cat|sys|ico|txt)",name,re.I) or name.lower() in names:
                raise ValueError("Unexpected package layout")
            names.add(name.lower())
            member=archive.read(entry)
            path=folder/name
            with path.open("xb") as output:
                output.write(member)
            locked=guard.enter_context(lock_binary(path))
            if locked.read()!=member:
                raise ValueError("Package member changed")
        if {p.name.lower() for p in folder.iterdir()}!=names:
            raise ValueError("Unexpected package contents")
        if powershell("cable_setup_install.ps1",folder,"Verify").returncode:
            raise ValueError("Vendor or driver catalog signature did not validate")
        yield folder


def launch(folder):
    # Installer runs to natural completion. Never terminate a driver installer mid-flight.
    return powershell("cable_setup_install.ps1",folder,"Launch",wait=None).returncode


class Controller:
    def __init__(self,probe=inspect_pair,prepare=package,install=launch,delay=time.sleep):
        self.probe,self.prepare,self.install,self.delay=probe,prepare,install,delay
        self.lock=threading.RLock()
        self.value=None
        self.checked=0.0
        self.thread=None

    def _set(self,value):
        with self.lock:
            self.value=value
            self.checked=time.monotonic()

    def status(self):
        with self.lock:
            if self.value is None or (self.value["state"] not in BUSY and time.monotonic()-self.checked>8):
                current=self.probe()
                if self.value and current["state"] in ("missing","partial") and self.value["state"] in ("failed","cancelled","restart-needed"):
                    current=self.value
                self._set(current)
            return dict(self.value)

    def begin(self):
        with self.lock:
            if self.thread and self.thread.is_alive():
                return self.status()
            current=self.probe()
            if current["state"]!="missing":
                self._set(current)
            elif self.value and self.value["state"]=="restart-needed":
                return dict(self.value)
            else:
                self._set(shape("preparing"))
                self.thread=threading.Thread(target=self._work,daemon=True)
                self.thread.start()
            return dict(self.value)

    def _work(self):
        try:
            with self.prepare() as folder:
                current=self.probe()
                if current["state"]!="missing":
                    self._set(current)
                    return
                self._set(shape("awaiting-windows"))
                code=self.install(folder)
                if code==1223:
                    self._set(shape("cancelled"))
                    return
                if code not in (0,3010,1641):
                    current=self.probe()
                    self._set(current if current["ready"] else shape("failed"))
                    return
            for _ in range(5):
                self.delay(1)
                current=self.probe()
                if current["ready"]:
                    self._set(current)
                    return
            self._set(shape("restart-needed"))
        except Exception:
            self._set(shape("failed"))


_controller=Controller()
status=_controller.status
begin=_controller.begin


if __name__=="__main__":
    parser=argparse.ArgumentParser()
    options=parser.add_mutually_exclusive_group()
    options.add_argument("--status",action="store_true");options.add_argument("--prepare",action="store_true");options.add_argument("--install",action="store_true")
    args=parser.parse_args()
    if args.prepare:
        with package():
            print("Original VB-Audio ZIP, member bytes, installer identity and driver catalog verified. No program launched.")
    else:
        result=begin() if args.install else status()
        while result["state"] in BUSY:
            time.sleep(.5)
            result=status()
        print(json.dumps(result))
