"""Prepare the official Windows desktop player after an explicit setup click."""
import ctypes
from http.client import HTTPException
import os
from pathlib import Path
import subprocess
import threading
import time
from urllib import request

import psutil
from cable_setup import Direct
from client_update import locked_installer
import diagnostics
from spotify_locations import known_process_path, store_installed, website_executable
from update_feed import plain


URL = "https://download.scdn.co/SpotifySetup.exe"
MAX_BYTES = 16 * 1024 * 1024
CHECK_SIGNATURE = """
$ErrorActionPreference = 'Stop'
$s = Get-AuthenticodeSignature -LiteralPath $env:LUMMIX_SPOTIFY_FILE
if ($s.Status -ne 'Valid' -or $null -eq $s.SignerCertificate -or
    $s.SignerCertificate.GetNameInfo('SimpleName', $false) -cne 'Spotify AB') { exit 1 }
"""


def installed():
    desktop = website_executable()
    if desktop:
        return desktop
    return "spotify:" if store_installed() else None


def running(installer=None):
    target_name = installer.name.casefold() if installer else "spotify.exe"
    session_id = ctypes.windll.kernel32.ProcessIdToSessionId
    current = ctypes.c_ulong()
    if not session_id(os.getpid(), ctypes.byref(current)):
        raise OSError("Cannot identify the current logon session")
    for process in psutil.process_iter(["name"]):
        if (process.info["name"] or "").casefold() != target_name:
            continue
        session = ctypes.c_ulong()
        if not session_id(process.pid, ctypes.byref(session)) or session.value != current.value:
            continue
        try:
            path = Path(process.exe())
            if installer is not None and path == installer:
                return True
            if installer is None and known_process_path(path, os.environ):
                return True
        except (psutil.Error, OSError):
            continue
    return False


def verify_publisher(path):
    shell = Path(os.environ["SystemRoot"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"
    env = dict(os.environ, PSModulePath=str(shell.parent / "Modules"), LUMMIX_SPOTIFY_FILE=str(path))
    check = subprocess.run([str(shell), "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", CHECK_SIGNATURE],
                           env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                           timeout=45, creationflags=subprocess.CREATE_NO_WINDOW)
    if check.returncode:
        raise ValueError("Windows could not verify Spotify AB as the publisher")


def download(path, cancelled, opener=None):
    opener = opener or request.build_opener(Direct()).open
    partial = path.with_suffix(".part")
    deadline = time.monotonic() + 90
    plain(path.parent)
    path.parent.mkdir(parents=True, exist_ok=True)
    for item in (path, partial):
        plain(item)
        item.unlink(missing_ok=True)
    try:
        with opener(URL, timeout=15) as response, partial.open("xb") as target:
            if response.status != 200 or response.geturl() != URL:
                raise ValueError("Unexpected Spotify download")
            length = response.headers.get("Content-Length")
            if length is not None and (not length.isdecimal() or not 0 < int(length) <= MAX_BYTES):
                raise ValueError("Invalid Spotify installer size")
            total = 0
            while True:
                if cancelled.is_set():
                    raise InterruptedError("Lummix closed")
                block = response.read(64 * 1024)
                if not block:
                    break
                total += len(block)
                if total > MAX_BYTES or time.monotonic() > deadline:
                    raise ValueError("Spotify download exceeded its limit")
                target.write(block)
            if not total or (length is not None and total != int(length)):
                raise ValueError("Incomplete Spotify installer")
        with open(str(partial) + ":Zone.Identifier", "w", encoding="ascii", newline="") as zone:
            zone.write(f"[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={URL}\r\n")
        partial.replace(path)
    finally:
        partial.unlink(missing_ok=True)


def open_verified(path, cancelled):
    with locked_installer(path):
        verify_publisher(path)
        if not cancelled.is_set():
            os.startfile(str(path), "open")


class SpotifySetup:
    def __init__(self, root, changed):
        self.path = Path(root) / "Spotify setup" / "SpotifySetup.exe"
        self.changed = changed
        self.lock = threading.Lock()
        self.cancelled = threading.Event()
        self.busy = False
        self.phase = "idle"
        self.cleanup()

    def cleanup(self):
        try:
            plain(self.path.parent)
            for item in (self.path.with_suffix(".part"), self.path):
                plain(item)
                item.unlink(missing_ok=True)
        except (OSError, ValueError):
            pass  # A vendor process may still have the single cached installer open.

    def status(self):
        with self.lock:
            return {"type":"lummix-spotify-setup", "phase":self.phase}

    def report(self, phase):
        with self.lock:
            self.phase = phase
        self.changed()

    def begin(self):
        with self.lock:
            if self.busy or self.cancelled.is_set():
                return
            self.busy = True
            self.phase = "checking"
        self.changed()
        threading.Thread(target=self._work, name="Lummix Spotify setup", daemon=True).start()

    def _work(self):
        phase = "failed"
        try:
            phase = self.prepare()
        except (OSError, ValueError, RuntimeError, AttributeError, ImportError, HTTPException, subprocess.SubprocessError):
            if not self.cancelled.is_set():
                diagnostics.record("spotify_setup_failed")
        finally:
            self.cleanup()
            with self.lock:
                self.busy = False
                self.phase = phase
            self.changed()

    def prepare(self):
        if running():
            return "ready"
        player = installed()
        if ctypes.windll.shell32.IsUserAnAdmin():
            return "elevated"  # Launch Spotify with the user's normal Windows token.
        if player:
            if self.cancelled.is_set():
                return "idle"
            if isinstance(player, Path):
                open_verified(player, self.cancelled)
            else:
                os.startfile(player, "open")
            return "opened"
        if not running(self.path):
            self.report("downloading")
            download(self.path, self.cancelled)
            if self.cancelled.is_set():
                return "idle"
            if installed() or running():
                return "ready"
            self.report("installing")
            open_verified(self.path, self.cancelled)
        # The website bootstrapper owns its process and login. Never stop or restart it.
        self.report("installing")
        deadline = time.monotonic() + 180
        while not self.cancelled.wait(2):
            if installed() or running():
                return "installed"
            if time.monotonic() >= deadline:
                return "unconfirmed"
        return "idle"
