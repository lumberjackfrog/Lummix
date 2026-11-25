"""Exercise the bundled driver's first-install flow on a disposable Windows Actions runner."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import subprocess
import sys
import time

import requests


def verify(exe):
    if os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted" or os.environ.get("RUNNER_OS") != "Windows":
        raise RuntimeError("This first-driver test requires a disposable GitHub-hosted Windows runner.")
    evidence = Path(".build/qa")
    evidence.mkdir(parents=True, exist_ok=True)
    headers = {"Host": "127.0.0.1:41836", "Origin": "http://127.0.0.1:41835"}
    base = "http://127.0.0.2:41836"
    states = []
    with requests.Session() as http, (evidence / "first-driver.log").open("wb") as log:
        http.trust_env = False

        def status():
            response = http.get(base + "/setup-status", headers=headers, timeout=25)
            response.raise_for_status()
            info = response.json()
            if not states or states[-1] != info["state"]:
                states.append(info["state"])
                print(info["state"], flush=True)
            return info

        @contextmanager
        def wrist():
            proc = subprocess.Popen([str(exe), "wrist", "--parent-stdin"], stdin=subprocess.PIPE,
                                    stdout=log, stderr=log, env=os.environ | {"LUMMIX_QA_LOOPBACK": "2"})
            try:
                for _ in range(100):
                    if proc.poll() is not None:
                        raise RuntimeError("Wrist service exited during startup")
                    try:
                        ready = http.get(base + "/lummix-health", headers=headers, timeout=1)
                        if ready.ok and ready.json().get("cableRestartSupport"):
                            break
                    except requests.ConnectionError:
                        time.sleep(.15)
                else:
                    raise RuntimeError("Wrist service did not become available")
                yield
            finally:
                proc.stdin.close()
                if proc.wait(timeout=30):
                    raise RuntimeError("Wrist service exited with an error")

        with wrist():
            first = status()
            assert first["state"] == "missing", first["state"]
            action = http.post(base + "/setup-cable", json={},
                               headers=headers | {"X-Lum-Setup-Token": first["token"]}, timeout=30)
            action.raise_for_status()
            deadline = time.monotonic() + 100
            while time.monotonic() < deadline:
                result = status()
                if result["state"] not in {"checking", "preparing", "awaiting-windows"}:
                    break
                time.sleep(1)
            assert result["state"] == "restart-needed" and not result["ready"], states
        with wrist():
            reopened = status()
            assert reopened["state"] == "restart-needed" and not reopened["ready"], states
        driver = subprocess.check_output([
            "powershell.exe", "-NoProfile", "-Command",
            "Get-CimInstance Win32_PnPSignedDriver -Filter \"DeviceName='VB-Audio Virtual Cable'\" | "
            "Where-Object {$_.IsSigned -and $_.DriverProviderName -eq 'VB-Audio Software'} | "
            "Select-Object -First 1 DeviceName,DriverVersion,IsSigned | ConvertTo-Json -Compress"
        ], text=True, timeout=30)
        # Windows can finish enumerating endpoints after reboot. Verify that Windows
        # has at least staged the vendor's driver package on the current boot.
        if not driver.strip():
            packages = subprocess.check_output(["pnputil.exe", "/enum-drivers"], text=True,
                                               errors="replace", timeout=30)
            assert "vbmme" in packages.lower() and "vb-audio" in packages.lower(), "Driver package missing"
        report = {"states": states, "afterAppRestart": reopened["state"],
                  "driver": json.loads(driver) if driver.strip() else "staged in Windows driver store"}
        (evidence / "first-driver.json").write_text(json.dumps(report, indent=2), encoding="utf-8")


if __name__ == "__main__":
    verify(Path(sys.argv[1]).resolve(strict=True))
