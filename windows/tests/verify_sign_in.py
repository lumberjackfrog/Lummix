"""Exercise the installed Settings switch and the exact command Windows runs at sign-in."""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import time
from urllib.error import URLError
from urllib.request import urlopen
import winreg

from playwright.sync_api import TimeoutError as BrowserTimeout, expect, sync_playwright
import win32con
import win32gui
import win32process


def run_entry():
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run") as key:
            return winreg.QueryValueEx(key, "Lummix")[0]
    except FileNotFoundError:
        return None


@contextmanager
def settings(command, profile, chromium, *, return_frame=False):
    with socket.socket() as reserved:
        reserved.bind(("127.0.0.1", 0))
        port = reserved.getsockname()[1]
    endpoint = f"http://127.0.0.1:{port}"
    env = dict(os.environ, LOCALAPPDATA=profile, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=f"--remote-debugging-port={port}")
    proc = subprocess.Popen(command, env=env)
    try:
        until = time.monotonic() + 45
        while time.monotonic() < until:
            assert proc.poll() is None, "The mixer exited before the sign-in test could open Settings"
            try:
                with urlopen(endpoint + "/json/list", timeout=1) as response:
                    if any(t.get("type")=="page" and t.get("url", "").startswith("http://127.0.0.1:41835/")
                           for t in json.load(response)):
                        break
            except (URLError, OSError):
                pass
            time.sleep(.2)
        else:
            raise RuntimeError("The native WebView did not open")
        browser = chromium.connect_over_cdp(endpoint)
        page = next(p for context in browser.contexts for p in context.pages if p.url.startswith("http://127.0.0.1:41835/"))
        page.bring_to_front()
        frame = page.frame_locator("#canvas")
        # The button is painted before the deferred native controls finish initializing.
        switch = frame.get_by_role("switch", name="Start with Windows")
        try:
            expect(frame.locator("html.native-window")).to_be_attached(timeout=30000)
            frame.get_by_role("button", name="Settings", exact=True).click(timeout=30000)
            expect(frame.locator("#settings-dialog[open]")).to_be_visible(timeout=30000)
            expect(switch).to_be_enabled(timeout=30000)
        except (AssertionError, BrowserTimeout):
            print("Settings readiness:", page.locator("#canvas").evaluate("""canvas => {
                const doc = canvas.contentDocument;
                return {document:doc?.readyState, visible:doc?.visibilityState,
                    native:doc?.documentElement.classList.contains('native-window'),
                    dialogOpen:doc?.getElementById('settings-dialog')?.open,
                    startupDisabled:doc?.getElementById('start-with-windows')?.disabled};
            }"""), flush=True)
            raise
        yield frame if return_frame else switch
    finally:
        if proc.poll() is None:
            hwnd = win32gui.FindWindow("LumMixStudioHost", None)
            if hwnd and win32process.GetWindowThreadProcessId(hwnd)[1] == proc.pid:
                win32gui.PostMessage(hwnd, win32con.WM_COMMAND, 0x7140, 0)
            try:
                proc.wait(12)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        assert proc.returncode == 0


def verify(exe, upgraded, with_updates=False):
    expected = f'"{exe}" --startup'
    assert run_entry() == (expected if upgraded else None)
    assert not win32gui.FindWindow("LumMixStudioHost", None), "Run on an idle test account"
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as profile, sync_playwright() as pw:
        with settings([str(exe)], profile, pw.chromium, return_frame=True) as frame:
            switch = frame.get_by_role("switch", name="Start with Windows")
            if with_updates:
                verify_update_controls(frame, profile)
            if upgraded:
                expect(switch).to_be_checked()
                switch.uncheck()
                expect(switch).to_be_enabled()
            expect(switch).not_to_be_checked()
            assert run_entry() is None
            switch.check()
            expect(switch).to_be_enabled()
            assert run_entry() == expected
        with settings(run_entry(), profile, pw.chromium) as switch:
            expect(switch).to_be_checked()
            switch.uncheck()
            expect(switch).to_be_enabled()
            assert run_entry() is None
        # A stale command queued before switching off must exit before opening the mixer.
        subprocess.run([str(exe), "--startup"], check=True, timeout=8)
    print("Verified Settings, the Windows Run value, enabled sign-in, and disabled sign-in.")


def verify_update_controls(frame, profile):
    from release_identity import VERSION
    expect(frame.locator("#app-version")).to_have_text("v" + VERSION)
    status = frame.locator("#update-status")
    ready = re.compile(r"^(?:You’re up to date\.|Version \d+\.\d+\.\d+ is available\.)$")
    expect(status).to_have_text(ready, timeout=45000)
    if status.inner_text() == "You’re up to date.":
        frame.get_by_role("button", name="Check for updates").click()
        expect(status).to_have_text(ready, timeout=45000)
    reported = re.fullmatch(r"Version (\d+\.\d+\.\d+) is available\.", status.inner_text())
    if reported:
        successor = reported.group(1)
        assert tuple(map(int, successor.split("."))) > tuple(map(int, VERSION.split(".")))
        expect(frame.get_by_role("button", name="Download " + successor)).to_be_enabled()
    frame.locator("#app-diagnostics > summary").click()
    log = Path(profile) / "LumMix/Diagnostics/lummix.log"
    assert "opened" in log.read_text()
    frame.get_by_role("button", name="Clear logs").click()
    expect(frame.locator("#logs-status")).to_have_text("Local logs cleared.")
    assert not log.exists()
    print("Verified native updater status, installed version and clearing disposable local diagnostics.")


def verify_updates(exe):
    assert not win32gui.FindWindow("LumMixStudioHost", None), "Run on an idle test account"
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as tmp, sync_playwright() as pw:
        with settings([str(exe)],tmp,pw.chromium,return_frame=True) as frame:
            verify_update_controls(frame, tmp)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("exe", type=Path)
    parser.add_argument("--upgraded", action="store_true")
    parser.add_argument("--updates", action="store_true")
    parser.add_argument("--with-updates", action="store_true")
    args = parser.parse_args()
    exe=args.exe.resolve(strict=True)
    if args.updates: verify_updates(exe)
    else: verify(exe, args.upgraded, args.with_updates)
