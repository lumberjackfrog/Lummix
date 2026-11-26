"""The current user's Windows Startup Apps entry for the native mixer."""
import os
from pathlib import Path
import re
import subprocess
import sys
import winreg


RUN = r"Software\Microsoft\Windows\CurrentVersion\Run"
APPROVAL = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"
NAME = "Lummix"
NATIVE_STATES = {
    "enabled": (True, True, ""),
    "disabled": (False, True, ""),
    "unavailable": (False, False, "Available in the Windows app."),
    "occupied": (False, False, "Another startup entry already uses the name Lummix."),
    "blocked": (False, False, "Windows has disabled Lummix. You can enable it in Settings → Apps → Startup."),
}


class SignIn:
    def __init__(self, executable=None, hive=winreg.HKEY_CURRENT_USER, install_root=None):
        self.exe = Path(executable or sys.executable).resolve()
        self.hive = hive
        self.install_root = install_root or Path(os.environ["LOCALAPPDATA"]) / "Programs" / NAME
        self.native = (getattr(sys, "frozen", False) and executable is None
                       and hive == winreg.HKEY_CURRENT_USER and install_root is None)

    def _native(self, action):
        host = self.exe.with_name("Lummix Host.exe")
        result = subprocess.run([str(host), "sign-in", action, str(self.exe)], capture_output=True,
                                stdin=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
        if result.returncode == 5:
            raise PermissionError("Windows blocked access to the Lummix startup entry.")
        if result.returncode:
            raise OSError("The Lummix host could not access the startup entry.")
        if action == "refresh":
            return
        try:
            enabled, editable, message = NATIVE_STATES[result.stdout.strip().decode("ascii")]
        except (KeyError, UnicodeError) as error:
            raise OSError("Invalid startup status from the Lummix host.") from error
        return dict(enabled=enabled, editable=editable, message=message)

    def read(self, key):
        try:
            with winreg.OpenKey(self.hive, key) as handle:
                return winreg.QueryValueEx(handle, NAME)[0]
        except FileNotFoundError:
            return None

    def owns(self, value):
        if not isinstance(value, str):
            return False
        match = re.fullmatch(r'"([^"]+)"(?: --startup)?', value)
        target = Path(match[1] if match else value)
        if target == self.exe:
            return True
        return (target.name.lower() == "lummix.exe" and self.exe.name.lower() == "lummix.exe"
                and target.parent.parent == self.exe.parent.parent == self.install_root
                and all(re.fullmatch(r"\d+\.\d+\.\d+", path.parent.name) for path in (target, self.exe)))

    def state(self):
        if self.native:
            return self._native("state")
        if self.exe.name.lower() != "lummix.exe":
            return dict(enabled=False, editable=False, message="Available in the Windows app.")
        value = self.read(RUN)
        if value is not None and not self.owns(value):
            return dict(enabled=False, editable=False, message="Another startup entry already uses the name Lummix.")
        approval = self.read(APPROVAL) if value else None
        if isinstance(approval, bytes) and approval[:1] in (b"\x03", b"\x07"):
            return dict(enabled=False, editable=False, message="Windows has disabled Lummix. You can enable it in Settings → Apps → Startup.")
        return dict(enabled=value is not None, editable=True, message="")

    def change(self, enabled):
        if self.native:
            return self._native("enable" if enabled else "disable")
        if self.exe.name.lower() != "lummix.exe":
            return self.state()
        value = self.read(RUN)
        if value is not None and not self.owns(value):
            return self.state()
        if enabled:
            with winreg.CreateKeyEx(self.hive, RUN, 0, winreg.KEY_SET_VALUE) as handle:
                winreg.SetValueEx(handle, NAME, 0, winreg.REG_SZ, f'"{self.exe}" --startup')
        elif value is not None:
            with winreg.OpenKey(self.hive, RUN, 0, winreg.KEY_SET_VALUE) as handle:
                try:
                    winreg.DeleteValue(handle, NAME)
                except FileNotFoundError:
                    pass
        return self.state()

    def refresh_after_upgrade(self):
        if self.native:
            return self._native("refresh")
        if self.owns(self.read(RUN)):
            self.change(True)
