"""Compare shipped Windows launchers on a disposable interactive hosted runner."""
import ctypes
from ctypes import wintypes
import hashlib
from http.client import HTTPConnection
import os
from pathlib import Path
import socket
from statistics import median
import subprocess
import tempfile
import time
import uuid

from playwright.sync_api import Error as PlaywrightError, sync_playwright
import psutil
import win32con
import win32event
import win32gui
import win32process


PORTS = {41835, 41836, 41838, 41839}
WORKING_SET = "rss"  # Windows PROCESS_MEMORY_COUNTERS.WorkingSetSize
DESKTOP_CLASS = "LumMixStudioHost"
EXIT_COMMAND = 0x7140
START_TIMEOUT = 50
POLL_SECONDS = .04


def _listeners():
    return {entry.laddr.port: entry.pid for entry in psutil.net_connections(kind="tcp")
            if entry.status == psutil.CONN_LISTEN and entry.laddr.port in PORTS}


def _wait_until(predicate, seconds, failure):
    deadline = time.perf_counter() + seconds
    while time.perf_counter() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(POLL_SECONDS)
    raise AssertionError(failure)


def _find_window(desktop):
    windows = []

    def collect(hwnd, _):
        title = win32gui.GetWindowText(hwnd)
        classname = win32gui.GetClassName(hwnd)
        if title == "Lummix" and classname == "#32770":
            raise RuntimeError("Lummix opened a modal instead of initializing its mixer")
        if (classname == DESKTOP_CLASS and title.startswith("Lummix")
                and win32gui.IsWindowVisible(hwnd) and not win32gui.IsIconic(hwnd)):
            windows.append(hwnd)
        return True

    win32gui.EnumDesktopWindows(desktop, collect, None)
    assert len(windows) <= 1, windows
    return windows[0] if windows else None


def _endpoint_ready(port):
    connection = HTTPConnection("127.0.0.1", port, timeout=.15)
    try:
        connection.request("GET", "/json/version")
        return connection.getresponse().status == 200
    except OSError:
        return False
    finally:
        connection.close()


def _owns_image(exe, process):
    return Path(process.exe()).resolve().is_relative_to(exe.parent)


def _snapshot(exe, window_pid, renderer_profile):
    owned = {}
    webview = {}
    bound = _listeners()
    assert {41835, 41836, 41838} <= bound.keys(), bound
    for pid in set(bound.values()) | {window_pid}:
        assert pid and _owns_image(exe, psutil.Process(pid)), (exe, pid, bound)
    for process in psutil.process_iter(["pid", "name", "exe"]):
        try:
            if process.info["exe"] and _owns_image(exe, process):
                owned[process.pid] = process
            elif (process.info["name"] or "").casefold() == "msedgewebview2.exe":
                # All WebView processes inherit ancestry from the renderer profile owner.
                if any(str(renderer_profile).casefold() in arg.casefold() for arg in process.cmdline()):
                    webview[process.pid] = process
                    webview.update((child.pid, child) for child in process.children(recursive=True)
                                   if child.name().casefold() == "msedgewebview2.exe")
        except psutil.NoSuchProcess:
            continue
    # Parent traversal also covers the prior launcher when its WebView uses a
    # default user-data location and subprocesses omit --user-data-dir.
    for process in owned.values():
        for child in process.children(recursive=True):
            try:
                if child.name().casefold() == "msedgewebview2.exe":
                    webview[child.pid] = child
            except psutil.NoSuchProcess:
                pass
    assert window_pid in owned and webview, (window_pid, list(owned), list(webview))

    def sample(processes):
        rows = []
        for pid, process in sorted(processes.items()):
            try:
                rows.append({"pid": pid, "image": process.name(),
                             "workingSetBytes": getattr(process.memory_info(), WORKING_SET)})
            except psutil.NoSuchProcess:
                continue
        return rows

    return sample(owned), sample(webview), set(owned) | set(webview)


def _started_processes(exe):
    return [process for process in psutil.process_iter(["exe"])
            if process.info["exe"] and Path(process.info["exe"]).is_relative_to(exe.parent)]


def _trial(exe, version, number, work_dir, chromium):
    assert not _listeners(), "Lummix benchmark requires unused product service ports"
    assert not _started_processes(exe), f"An executable is already running from {exe.parent}"
    u = ctypes.WinDLL("user32", use_last_error=True)
    u.CreateDesktopW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, ctypes.c_void_p, wintypes.DWORD,
                                wintypes.DWORD, ctypes.c_void_p]
    u.CreateDesktopW.restype = wintypes.HANDLE
    u.CloseDesktop.argtypes = [wintypes.HANDLE]
    desktop_name = "LummixBench_" + uuid.uuid4().hex
    desktop = u.CreateDesktopW(desktop_name, None, None, 0, 0x000F01FF, None)
    if not desktop:
        raise ctypes.WinError(ctypes.get_last_error())
    process = thread = browser = None
    hwnd = None
    tracked = {}
    try:
        with tempfile.TemporaryDirectory(prefix=f"{version}-{number}-", dir=work_dir,
                                         ignore_cleanup_errors=True) as scratch:
            profile = Path(scratch).resolve()
            local = profile / "local"
            local.mkdir()
            renderer_profile = profile / "webview"
            with socket.socket() as probe:
                probe.bind(("127.0.0.1", 0))
                debug_port = probe.getsockname()[1]
            system = Path(os.environ["SystemRoot"])
            env = dict(os.environ, LOCALAPPDATA=str(local), WEBVIEW2_USER_DATA_FOLDER=str(renderer_profile),
                       WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=f"--remote-debugging-port={debug_port}",
                       PATH=os.pathsep.join(map(str, (system / "System32", system))))
            for variable in ("PYTHONHOME", "PYTHONPATH", "VIRTUAL_ENV", "LUMMIX_QA_LOOPBACK",
                             "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER"):
                env.pop(variable, None)
            startup = win32process.STARTUPINFO()
            startup.lpDesktop = "winsta0\\" + desktop_name
            begin = time.perf_counter()
            process, thread, _, _ = win32process.CreateProcess(
                str(exe), subprocess.list2cmdline([str(exe)]), None, None, False,
                win32con.CREATE_UNICODE_ENVIRONMENT | win32con.CREATE_NO_WINDOW,
                env, str(exe.parent), startup)
            hwnd = _wait_until(lambda: _find_window(desktop), START_TIMEOUT, "No visible Lummix window")
            visible_ms = (time.perf_counter() - begin) * 1000
            def remaining():
                return max(.01, START_TIMEOUT - (time.perf_counter() - begin))
            _wait_until(lambda: _endpoint_ready(debug_port), remaining(), "WebView2 CDP did not start")
            browser = chromium.connect_over_cdp(f"http://127.0.0.1:{debug_port}", timeout=remaining() * 1000)

            def mixer_ready():
                for context in browser.contexts:
                    for page in context.pages:
                        if not page.url.startswith("http://127.0.0.1:41835/"):
                            continue
                        try:
                            if page.evaluate("""() => {
                                const frame = document.querySelector('#canvas');
                                const mixer = frame?.contentDocument;
                                const visualizer = mixer?.querySelector('#lum-visualizer');
                                return Boolean(mixer?.readyState === 'complete' && visualizer?.getClientRects().length
                                    && typeof frame.contentWindow.releaseLummixAudio === 'function');
                            }"""):
                                return page
                        except PlaywrightError:
                            continue
                return None

            _wait_until(mixer_ready, remaining(), "The real Studio mixer did not become ready")
            ready_ms = (time.perf_counter() - begin) * 1000
            children = []

            def child_class(child, _):
                children.append(win32gui.GetClassName(child))
                return True

            win32gui.EnumChildWindows(hwnd, child_class, None)
            assert any("Chrome" in item for item in children), children
            window_pid = win32process.GetWindowThreadProcessId(hwnd)[1]
            product, webview, ids = _snapshot(exe, window_pid, renderer_profile)
            tracked = {item: psutil.Process(item) for item in ids if psutil.pid_exists(item)}
            assert product and webview
            win32gui.PostMessage(hwnd, win32con.WM_COMMAND, EXIT_COMMAND, 0)
            assert win32event.WaitForSingleObject(process, 15000) == win32con.WAIT_OBJECT_0
            assert win32process.GetExitCodeProcess(process) == 0
            _wait_until(lambda: not _listeners() and not _started_processes(exe)
                        and not any(item.is_running() for item in tracked.values()), 12,
                        "Lummix or WebView processes survived the normal shutdown")
            return {"version": version, "trial": number, "visibleWindowMs": round(visible_ms, 2),
                    "desktopReadyMs": round(ready_ms, 2), "windowPid": window_pid,
                    "lummixWorkingSetBytes": sum(row["workingSetBytes"] for row in product),
                    "webViewWorkingSetBytes": sum(row["workingSetBytes"] for row in webview),
                    "lummixProcesses": product, "webViewProcesses": webview, "shutdownClean": True}
    finally:
        if hwnd and win32gui.IsWindow(hwnd):
            win32gui.PostMessage(hwnd, win32con.WM_COMMAND, EXIT_COMMAND, 0)
        lingering = {item.pid: item for item in _started_processes(exe)} | tracked
        for item in lingering.values():
            if item.is_running():
                try:
                    if item.pid not in tracked or _owns_image(exe, item):
                        for child in item.children(recursive=True):
                            try:
                                child.kill()
                            except psutil.NoSuchProcess:
                                pass
                    item.kill()
                except psutil.NoSuchProcess:
                    pass
        if browser:
            try:
                browser.close()
            except PlaywrightError:
                pass
        if process:
            process.Close()
        if thread:
            thread.Close()
        u.CloseDesktop(desktop)


def compare_desktop(before_exe: Path, after_exe: Path, work_dir: Path) -> dict:
    """Compare no-argument launches of extracted payloads. Working-set sums may share DLL pages."""
    if os.name != "nt" or os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise RuntimeError("Desktop benchmarking requires a disposable GitHub-hosted Windows runner")
    before_exe, after_exe = (path.resolve(strict=True) for path in (before_exe, after_exe))
    assert before_exe.name == after_exe.name == "Lummix.exe"
    assert not before_exe.parent.is_relative_to(after_exe.parent)
    assert not after_exe.parent.is_relative_to(before_exe.parent)
    work_dir.mkdir(parents=True, exist_ok=True)
    cases = [("before", before_exe), ("after", after_exe)]
    trials = []
    with sync_playwright() as playwright:
        # Balanced order across four pairs controls for cache warming and runner drift.
        for number in range(1, 5):
            for version, exe in cases[::1 if number % 2 else -1]:
                trials.append(_trial(exe, version, number, work_dir, playwright.chromium))
    fields = ("visibleWindowMs", "desktopReadyMs", "lummixWorkingSetBytes", "webViewWorkingSetBytes")
    summaries = {}
    for version, exe in cases:
        samples = [trial for trial in trials if trial["version"] == version]
        summaries[version] = {
            "executableSha256": hashlib.sha256(exe.read_bytes()).hexdigest(),
            **{key: {name: aggregate(sample[key] for sample in samples)
                     for name, aggregate in (("median", median), ("min", min), ("max", max))}
               for key in fields},
        }
    return {"trials": trials, "summary": summaries, "pairCount": 4,
            "environment": "fresh per-user and Evergreen WebView2 profiles on a private Win32 desktop",
            "workingSet": "Summed per-process RSS bytes at mixer readiness, shared pages may count more than once"}
