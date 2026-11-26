"""Verify cold start, reuse and conflicts through the packaged Rust lifecycle."""
from contextlib import ExitStack, contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
from queue import Queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import zipfile

import psutil

from native_qa import STAGED, verify_bundle, verify_listener
from test_runtime import MEDIA_CAPABILITIES

EVIDENCE = STAGED.parents[3] / ".build/qa"
ADDRESS = "127.0.0.2"
PORTS = (41835, 41836, 41838)
ORIGIN = "http://127.0.0.1:41835"


def listeners():
    return {entry.laddr.port: entry.pid for entry in psutil.net_connections(kind="tcp")
            if entry.status == psutil.CONN_LISTEN and entry.laddr.ip == ADDRESS
            and entry.laddr.port in PORTS}


@contextmanager
def fixture(port, body):
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header("Access-Control-Allow-Origin", ORIGIN)
            self.end_headers()
            self.wfile.write(body() if callable(body) else body)

        def log_message(self, *_args):
            pass

    # Do not allow a second process to bind an occupied Windows listener.
    class ExclusiveServer(ThreadingHTTPServer):
        allow_reuse_address = False

    with ExclusiveServer((ADDRESS, port), Handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield
        finally:
            server.shutdown()
            thread.join()


@contextmanager
def launch(exe, env, cwd, log, expected=True):
    args = [str(exe), "verify-services", "--parent-stdin"]
    with subprocess.Popen(args, env=env, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=log, text=True, creationflags=subprocess.CREATE_NO_WINDOW) as child:
        lines = Queue()
        threading.Thread(target=lambda: lines.put(child.stdout.readline()), daemon=True).start()
        try:
            first = lines.get(timeout=35)
            if expected:
                assert json.loads(first) == {"service": "lummix-lifecycle", "ready": True}, first
                yield child
            else:
                assert not first and child.wait(timeout=10) != 0, first
                yield child
        finally:
            child.stdin.close()
            try:
                code = child.wait(timeout=12)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
                raise
        if expected:
            assert code == 0, code


@contextmanager
def standalone_service(exe, env, cwd, log, mode, port):
    args = [str(exe), mode, "--parent-stdin"]
    with subprocess.Popen(args, env=env, cwd=cwd, stdin=subprocess.PIPE, stdout=log,
                          stderr=log, creationflags=subprocess.CREATE_NO_WINDOW) as child:
        try:
            deadline = time.monotonic() + 20
            while listeners().get(port) != child.pid:
                assert child.poll() is None and time.monotonic() < deadline, (port, child.returncode)
                time.sleep(.15)
            verify_listener(exe, child.pid, ADDRESS, port)
            yield child
        finally:
            child.stdin.close()
            try:
                assert child.wait(timeout=12) == 0
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
                raise


def run():
    EVIDENCE.mkdir(parents=True, exist_ok=True)
    assert not listeners(), "The QA-only loopback address already has a Lummix listener"
    with tempfile.TemporaryDirectory(prefix="LummixCold-") as scratch:
        cwd = Path(scratch)
        chosen = cwd / "Extracted beside a space"
        if len(sys.argv) > 1:
            with zipfile.ZipFile(sys.argv[1]) as archive:
                archive.extractall(chosen)
            exe = chosen / "Lummix/Lummix.exe"
        else:
            shutil.copytree(STAGED.parent, chosen)
            exe = chosen / STAGED.name
        verify_bundle(exe)
        env = dict(os.environ, LUMMIX_QA_LOOPBACK="2", LOCALAPPDATA=str(cwd / "local"),
                   PATH=os.environ["SystemRoot"] + r"\System32")
        for var in ("PYTHONHOME", "PYTHONPATH", "VIRTUAL_ENV"):
            env.pop(var, None)
        with (EVIDENCE / "cold-launch.log").open("wb") as log:
            with launch(exe, env, cwd, log) as app:
                owned = listeners()
                assert set(owned) == set(PORTS), owned
                assert owned[41835] == app.pid
                for port, pid in owned.items():
                    verify_listener(exe, pid, ADDRESS, port)
            assert not listeners()
            with ExitStack() as services:
                reused = {port: services.enter_context(
                    standalone_service(exe, env, cwd, log, mode, port)).pid
                    for port, mode in zip(PORTS, ("studio", "wrist", "spotify"))}
                with launch(exe, env, cwd, log):
                    assert listeners() == reused
                assert listeners() == reused
            assert not listeners()

            with fixture(41835, b"other app"):
                with launch(exe, env, cwd, log, expected=False):
                    assert listeners() == {41835: os.getpid()}

            health = {"service": "lummix-media", "protocol": 1,
                      "capabilities": ["spotify-player-volume-v1"]}
            with fixture(41838, lambda: json.dumps(health).encode()):
                for version in (1, 2, 3):
                    health["protocol"] = version
                    with launch(exe, env, cwd, log, expected=False):
                        assert listeners() == {41838: os.getpid()}
                caps = MEDIA_CAPABILITIES | {"spotify-smtc-rust-v1"}
                health.update(protocol=2, capabilities=sorted(caps))
                for extra in ([], ["future-feature"]):
                    health["capabilities"] = sorted(caps) + extra
                    with launch(exe, env, cwd, log):
                        assert listeners()[41838] == os.getpid()
                    assert listeners() == {41838: os.getpid()}
            assert not listeners()
        result = {"coldServices": 3, "reused": 3, "wrongServiceRejected": True,
                  "ownedExitedCleanly": True, "staleMediaRejected": True,
                  "optionalPlayerVolumeNotRequired": True, "additionalCompatibleCapabilityAccepted": True,
                  "transport": "127.0.0.2; canonical Host and native lifecycle",
                  "helperSha256": hashlib.sha256(exe.read_bytes()).hexdigest()}
        print(json.dumps(result))
        (EVIDENCE / "cold-launch.json").write_text(json.dumps(result, indent=2), encoding="utf-8")


if __name__ == "__main__":
    run()
