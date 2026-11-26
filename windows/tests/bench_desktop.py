"""Compare shipped Windows launchers on a disposable interactive hosted runner."""
import hashlib
import json
from http.client import HTTPConnection
import os
from pathlib import Path
import socket
from statistics import median
import subprocess
import sys
import tempfile
import time
import traceback

from playwright.sync_api import Error as PlaywrightError, sync_playwright
import psutil
import win32api
import win32con
import win32gui
import win32job
import win32process


PORTS = {41835, 41836, 41838, 41839}
WORKING_SET = "rss"  # Windows PROCESS_MEMORY_COUNTERS.WorkingSetSize
DESKTOP_CLASS = "LumMixStudioHost"
EXIT_COMMAND = 0x7140
START_TIMEOUT = 50
TRIAL_WALL_TIMEOUT = 95
POLL_SECONDS = .04
STUDIO_URL = "http://127.0.0.1:41835/"


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


def _find_window():
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

    win32gui.EnumWindows(collect, None)
    assert len(windows) <= 1, windows
    return windows[0] if windows else None


def _studio_target(port):
    connection = HTTPConnection("127.0.0.1", port, timeout=.5)
    try:
        connection.request("GET", "/json/list")
        response = connection.getresponse()
        if response.status != 200:
            return None
        return next((target["url"] for target in json.load(response)
                     if target.get("type") == "page" and target.get("url", "").startswith(STUDIO_URL)), None)
    except (OSError, ValueError):
        return None
    finally:
        connection.close()


_MIXER_STATE = """() => {
    const frame = document.querySelector('#canvas');
    const mixer = frame?.contentDocument;
    const visible = node => {
        const r = node?.getBoundingClientRect();
        return Boolean(r?.width && r?.height
            && node.ownerDocument.defaultView.getComputedStyle(node).visibility === 'visible');
    };
    const art = mixer?.querySelector('#lum-visualizer');
    return {
        url: location.href,
        boardState: document.readyState,
        mixerState: mixer?.readyState,
        mixerTitle: mixer?.title,
        frame: visible(frame),
        mic: visible(mixer?.querySelector('#mic')),
        music: visible(mixer?.querySelector('#music')),
        artwork: Boolean(art?.querySelector('.lum-cover-image, img, canvas') || visible(art)),
        release: typeof frame?.contentWindow?.releaseLummixAudio === 'function',
    };
}"""
_MIXER_READY = f"""() => {{
    const state = ({_MIXER_STATE})();
    return state.url.startsWith('http://127.0.0.1:41835/')
        && state.frame && state.mic && state.music && state.artwork && state.release;
}}"""


def _wait_for_mixer(page, deadline):
    try:
        page.wait_for_function(_MIXER_READY, polling=40,
                               timeout=max(1, (deadline - time.perf_counter()) * 1000))
    except PlaywrightError as error:
        # The renderer may have stopped responding. Avoid a second renderer call
        # in the error path and report the last protocol-observed page state.
        detail = str(error).splitlines()[0]
        raise RuntimeError(f"Mixer readiness failed for {page.url!r}; live DOM state unavailable: {detail}") from error


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
    assert not _find_window(), "A Lummix window was already open on the runner desktop"
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
        _stage(version, number, "starting launcher")
        begin = time.perf_counter()
        process = subprocess.Popen([str(exe)], env=env, cwd=exe.parent)
        hwnd = _wait_until(_find_window, START_TIMEOUT, "No visible Lummix window")
        visible_ms = (time.perf_counter() - begin) * 1000
        window_pid = win32process.GetWindowThreadProcessId(hwnd)[1]
        assert _owns_image(exe, psutil.Process(window_pid)), (exe, window_pid)
        _stage(version, number, f"visible window at {visible_ms:.0f} ms")

        def remaining():
            return max(.01, START_TIMEOUT - (time.perf_counter() - begin))

        target = _wait_until(lambda: _studio_target(debug_port), remaining(), "WebView2 did not advertise a Studio page")
        _stage(version, number, f"Studio URL advertised: {target}")
        browser = chromium.connect_over_cdp(f"http://127.0.0.1:{debug_port}", timeout=remaining() * 1000)
        _stage(version, number, "CDP attached")
        page = next((page for context in browser.contexts for page in context.pages
                     if page.url.startswith(STUDIO_URL)), None)
        if page is None:
            urls = [page.url for context in browser.contexts for page in context.pages]
            raise RuntimeError(f"CDP attached without the advertised Studio page {target!r}: {urls}")
        page.bring_to_front()
        _wait_for_mixer(page, begin + START_TIMEOUT)
        ready_ms = (time.perf_counter() - begin) * 1000
        _stage(version, number, f"mixer ready at {ready_ms:.0f} ms, checking ownership")
        children = []

        def child_class(child, _):
            children.append(win32gui.GetClassName(child))
            return True

        win32gui.EnumChildWindows(hwnd, child_class, None)
        assert any("Chrome" in item for item in children), children
        product, webview, ids = _snapshot(exe, window_pid, renderer_profile)
        tracked = {item: psutil.Process(item) for item in ids if psutil.pid_exists(item)}
        assert product and webview
        _stage(version, number, "requesting normal exit")
        win32gui.PostMessage(hwnd, win32con.WM_COMMAND, EXIT_COMMAND, 0)
        assert process.wait(timeout=15) == 0, process.returncode
        _wait_until(lambda: not _listeners() and not _started_processes(exe)
                    and not any(item.is_running() for item in tracked.values()), 12,
                    "Lummix or WebView processes survived the normal shutdown")
        _stage(version, number, "launcher, services and WebView exited cleanly")
        return {"version": version, "trial": number, "visibleWindowMs": round(visible_ms, 2),
                "desktopReadyMs": round(ready_ms, 2), "windowPid": window_pid,
                "lummixWorkingSetBytes": sum(row["workingSetBytes"] for row in product),
                "webViewWorkingSetBytes": sum(row["workingSetBytes"] for row in webview),
                "lummixProcesses": product, "webViewProcesses": webview, "shutdownClean": True}


def _stage(version, number, message):
    line = f"[desktop {version} {number}] {message}"
    print(line, flush=True)
    if log := os.environ.get("LUMMIX_BENCH_TRIAL_LOG"):
        with Path(log).open("a", encoding="utf-8") as stream:
            print(line, file=stream)


def _require_hosted_runner():
    if (os.name != "nt" or os.environ.get("GITHUB_ACTIONS") != "true"
            or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"):
        raise RuntimeError("Desktop benchmarking requires a disposable GitHub-hosted Windows runner")


def _worker(exe, version, number, work_dir, result_path):
    _require_hosted_runner()
    # The supervisor assigns the worker to the Job before releasing this gate.
    if sys.stdin.buffer.read(1) != b"1":
        raise RuntimeError("Desktop trial did not receive its Windows Job")
    _stage(version, number, "worker running")
    with sync_playwright() as playwright:
        result = _trial(exe, version, number, work_dir, playwright.chromium)
        pending = result_path.with_suffix(".tmp")
        with pending.open("w", encoding="utf-8") as stream:
            json.dump(result, stream, indent=2)
            stream.flush()
            os.fsync(stream.fileno())
        pending.replace(result_path)
        _stage(version, number, f"saved result to {result_path}")
    _stage(version, number, "worker finished")


def _run_trial(exe, version, number, work_dir, evidence_dir):
    result_path = evidence_dir / f"{version}-{number}.json"
    if result_path.exists():
        raise FileExistsError(result_path)
    job = win32job.CreateJobObject(None, "")
    try:
        limits = win32job.QueryInformationJobObject(job, win32job.JobObjectExtendedLimitInformation)
        limits["BasicLimitInformation"]["LimitFlags"] |= win32job.JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        win32job.SetInformationJobObject(job, win32job.JobObjectExtendedLimitInformation, limits)
        command = [sys.executable, "-u", str(Path(__file__).resolve()), "--trial-worker",
                   str(exe), version, str(number), str(work_dir), str(result_path)]
        _stage(version, number, f"starting worker with {TRIAL_WALL_TIMEOUT}-second limit")
        log_path = result_path.with_suffix(".log")
        log_path.write_text(f"Trial limit: {TRIAL_WALL_TIMEOUT} seconds\n", encoding="utf-8")
        worker = subprocess.Popen(command, stdin=subprocess.PIPE,
                                  env=dict(os.environ, LUMMIX_BENCH_TRIAL_LOG=str(log_path)))
        assigned = False
        try:
            access = win32con.PROCESS_SET_QUOTA | win32con.PROCESS_TERMINATE
            process = win32api.OpenProcess(access, False, worker.pid)
            try:
                win32job.AssignProcessToJobObject(job, process)
                assigned = True
            finally:
                process.Close()
            worker.stdin.write(b"1")
            worker.stdin.close()
            try:
                code = worker.wait(timeout=TRIAL_WALL_TIMEOUT)
            except subprocess.TimeoutExpired as error:
                _stage(version, number, "hard timeout, ending trial job")
                with log_path.open("a", encoding="utf-8") as stream:
                    print(f"Hard timeout after {TRIAL_WALL_TIMEOUT}s", file=stream)
                raise RuntimeError(f"Desktop {version} trial {number} exceeded {TRIAL_WALL_TIMEOUT}s; see {log_path}") from error
            if code != 0:
                raise RuntimeError(f"Desktop {version} trial {number} worker exited with code {code}")
            return json.loads(result_path.read_text(encoding="utf-8"))
        finally:
            worker.stdin.close()
            if worker.poll() is None:
                if assigned:
                    win32job.TerminateJobObject(job, 1)
                else:
                    worker.kill()  # No launch was possible before assignment.
                worker.wait(timeout=5)
    finally:
        job.Close()


def compare_desktop(before_exe: Path, after_exe: Path, work_dir: Path, evidence_dir: Path | None = None) -> dict:
    """Compare no-argument launches of extracted payloads. Working-set sums may share DLL pages."""
    _require_hosted_runner()
    before_exe, after_exe = (path.resolve(strict=True) for path in (before_exe, after_exe))
    assert before_exe.name == after_exe.name == "Lummix.exe"
    assert not before_exe.parent.is_relative_to(after_exe.parent)
    assert not after_exe.parent.is_relative_to(before_exe.parent)
    work_dir.mkdir(parents=True, exist_ok=True)
    work_dir = work_dir.resolve(strict=True)
    evidence_dir = (evidence_dir or work_dir).resolve() / "desktop-trials"
    evidence_dir.mkdir(parents=True, exist_ok=True)
    cases = [("before", before_exe), ("after", after_exe)]
    trials = []
    # Balanced order across four pairs controls for cache warming and runner drift.
    for number in range(1, 5):
        for version, exe in cases[::1 if number % 2 else -1]:
            trials.append(_run_trial(exe, version, number, work_dir, evidence_dir))
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
            "environment": "fresh per-user and Evergreen WebView2 profiles on the hosted runner desktop",
            "workingSet": "Summed per-process RSS bytes at mixer readiness, shared pages may count more than once"}


if __name__ == "__main__":
    mode, exe, version, number, work_dir, result = sys.argv[1:]
    if mode != "--trial-worker":
        raise SystemExit("This module is invoked by the runtime benchmark")
    try:
        _worker(Path(exe), version, int(number), Path(work_dir), Path(result))
    except Exception:
        if log := os.environ.get("LUMMIX_BENCH_TRIAL_LOG"):
            with Path(log).open("a", encoding="utf-8") as stream:
                traceback.print_exc(file=stream)
        raise
