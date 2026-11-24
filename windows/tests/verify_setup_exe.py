"""Run the shipped EXE without flags on a private desktop, on a clean Windows test account."""
import ctypes
from ctypes import wintypes
from hashlib import sha256
import json
import msilib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid
import zipfile

import psutil
import win32con
import win32event
import win32gui
import win32process
from win32com.client import Dispatch
from win32com.shell import shell as shell_api, shellcon


def checksum(path):
    with path.open("rb") as stream:
        return sha256(stream.read()).hexdigest()


def main(setup, silent=False):
    setup = setup.resolve(strict=True)
    archive = setup.with_name("Lummix for Windows.zip")
    msi = setup.with_suffix(".msi")
    product_state = ctypes.windll.msi.MsiQueryProductStateW
    product_state.argtypes = [wintypes.LPCWSTR]
    db = msilib.OpenDatabase(str(msi), msilib.MSIDBOPEN_READONLY)
    view = db.OpenView("SELECT Value FROM Property WHERE Property = 'ProductCode'")
    view.Execute(None)
    product = view.Fetch().GetString(1)
    view.Close()
    del view, db
    cache = Path(os.environ["LOCALAPPDATA"]) / "Lummix/Installer" / product / "Lummix.msi"
    programs = Path(os.environ["LOCALAPPDATA"]) / "Programs/Lummix"
    shell = Dispatch("WScript.Shell")
    shortcut = Path(shell_api.SHGetFolderPath(0, shellcon.CSIDL_PROGRAMS, None, 0)) / "Lummix/Lummix.lnk"
    desktop_link = Path(shell_api.SHGetFolderPath(0, shellcon.CSIDL_DESKTOPDIRECTORY, None, 0)) / "Lummix.lnk"
    if product_state(product) > 0 or programs.exists() or shortcut.exists() or desktop_link.exists() or cache.exists():
        raise RuntimeError("Run on a clean test account to preserve the existing Lummix installation.")

    user = ctypes.WinDLL("user32", use_last_error=True)
    user.CreateDesktopW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, ctypes.c_void_p,
                                    wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p]
    user.CreateDesktopW.restype = wintypes.HANDLE
    user.CloseDesktop.argtypes = [wintypes.HANDLE]
    title = "LummixSetupQA_" + uuid.uuid4().hex
    desktop = None if silent else user.CreateDesktopW(title, None, None, 0, 0x000F01FF, None)
    if not silent and not desktop:
        raise ctypes.WinError(ctypes.get_last_error())
    windows = []
    def find_studio(hwnd, _):
        if win32gui.GetClassName(hwnd) == "LumMixStudioHost" and win32gui.IsWindowVisible(hwnd):
            windows.append(hwnd)
        return True

    try:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as profile:
            if silent:
                subprocess.run([str(setup), "/S"], check=True, timeout=120)
            else:
                startup = win32process.STARTUPINFO()
                startup.lpDesktop = "winsta0\\" + title
                env = dict(os.environ, LOCALAPPDATA=profile)
                process, thread, _, _ = win32process.CreateProcess(str(setup), subprocess.list2cmdline([str(setup)]),
                    None, None, False, win32con.CREATE_UNICODE_ENVIRONMENT, env, str(setup.parent), startup)
                try:
                    assert win32event.WaitForSingleObject(process, 120000) == 0, "Setup did not finish"
                    assert win32process.GetExitCodeProcess(process) == 0, "Setup reported an installation error"
                finally:
                    thread.Close()
                    process.Close()
            assert product_state(product) == 5 and shortcut.is_file()
            executable = Path(shell.CreateShortcut(str(shortcut)).TargetPath)
            assert executable.is_relative_to(programs) and executable.is_file()
            assert desktop_link.is_file() and Path(shell.CreateShortcut(str(desktop_link)).TargetPath) == executable
            if not silent:
                deadline = time.monotonic() + 30
                while not windows and time.monotonic() < deadline:
                    win32gui.EnumDesktopWindows(desktop, find_studio, None)
                    if not windows:
                        time.sleep(.15)
                assert len(windows) == 1, "Default install did not open the Lummix desktop window"
                pid = win32process.GetWindowThreadProcessId(windows[0])[1]
                studio = psutil.Process(pid)
                assert Path(studio.exe()).samefile(executable)
                win32gui.PostMessage(windows[0], win32con.WM_COMMAND, 0x7140, 0)
                studio.wait(timeout=20)
            with zipfile.ZipFile(archive) as z:
                for entry in z.infolist():
                    path = executable.parent / Path(entry.filename).relative_to("Lummix")
                    assert path.read_bytes() == z.read(entry), entry.filename
                payload_count = len(z.infolist())
            assert checksum(cache) == checksum(msi)
            readme = executable.parent / "Read me.txt"
            assert readme.is_relative_to(programs) and readme.exists()
            readme.unlink()
            subprocess.run(["msiexec.exe", "/fp", product, "/qn", "/norestart"], check=True, timeout=120)
            assert readme.exists(), "Windows Installer could not repair from its cached source"
    finally:
        if product_state(product) > 0:
            subprocess.run(["msiexec.exe", "/x", product, "/qn", "/norestart"], check=True, timeout=120)
        if desktop:
            user.CloseDesktop(desktop)
    assert product_state(product) < 1 and not shortcut.exists() and not desktop_link.exists() and not cache.exists()
    print(json.dumps({"exeSha256": checksum(setup), "installedFiles": payload_count,
                      "normalLaunchOpenedStudio": None if silent else True, "repair": True,
                      "uninstallRemovedShortcutAndCachedMsi": True}, indent=2))


if __name__ == "__main__":
    main(Path(sys.argv[1]), silent="--silent" in sys.argv[2:])
