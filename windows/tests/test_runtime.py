"""Exercise packaged helpers from a different directory without a Python on PATH."""
import argparse
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import time
from urllib.parse import unquote

import requests
import psutil
from runtime_main import STUDIO_FILES
from lummix_window import MEDIA_CAPABILITIES

HERE = Path(__file__).resolve().parent
EVIDENCE = HERE.parents[1] / ".build/qa"
EVIDENCE.mkdir(parents=True, exist_ok=True)
UI = HERE.parents[1] / "studio"
TEST_PREFERENCES = {"snap": 5, "followTheme": False, "closeToTray": False}
TEST_GAINS = {"micGain": 72, "musicGain": 138, "micMuted": True, "musicMuted": False}


def verify_studio_assets(fetch, installed):
    checkout_line_endings = []
    for name in sorted(STUDIO_FILES):
        filename = unquote(name)
        packaged = (installed / filename).read_bytes()
        response = fetch(41835, "/" + name)
        assert response.status_code == 200 and response.content == packaged, (
            f"Studio did not serve the exact installed bytes for {filename}"
        )
        original = (UI / filename).read_bytes()
        if packaged != original:
            assert packaged.replace(b"\r\n", b"\n") == original.replace(b"\r\n", b"\n"), (
                f"Installed studio asset differs from checkout content for {filename}"
            )
            checkout_line_endings.append(filename)
    return checkout_line_endings


def verify_reinstall_profile(exe, phase):
    if os.environ.get("GITHUB_ACTIONS") != "true":
        raise RuntimeError("The reinstall profile check requires a fresh GitHub Actions runner.")
    profile = Path(os.environ["LOCALAPPDATA"]) / "LumMix"
    preferences = profile / "studio-preferences.json"
    gains = profile / "output-gains.json"
    backup = profile / "settings-backup.json"
    files = (preferences, gains, backup)
    if phase == "seed":
        assert not any(path.exists() for path in files), "Expected unused runner settings"
    else:
        assert all(path.is_file() for path in files), "Uninstall removed a seeded settings file"
        if phase == "recover":
            saved = json.loads(backup.read_bytes())["settings"][preferences.name]
            assert saved == json.loads(preferences.read_bytes()) == {"settings": TEST_PREFERENCES}
            preferences.write_bytes(b"{")

    log = EVIDENCE / f"profile-{phase}.log"
    system = Path(os.environ["SystemRoot"])
    env = os.environ | {"PATH": os.pathsep.join(map(str, (system / "System32", system))),
                        "LUMMIX_QA_LOOPBACK": "2"}
    for variable in ("PYTHONHOME", "PYTHONPATH", "VIRTUAL_ENV"):
        env.pop(variable, None)
    with requests.Session() as session, log.open("wb") as stream:
        session.trust_env = False
        headers = {"Host": "127.0.0.1:41838", "Origin": "http://127.0.0.1:41835"}

        def request(route, *, token=None, patch=None):
            url = "http://127.0.0.2:41838" + route
            if patch is None:
                response = session.get(url, headers=headers, timeout=5)
            else:
                response = session.post(url, headers=headers | {"X-Lum-Mix-Token": token}, json=patch, timeout=5)
            response.raise_for_status()
            return response.json()

        child = subprocess.Popen([str(exe), "spotify", "--parent-stdin"], env=env, stdin=subprocess.PIPE,
                                 stdout=stream, stderr=stream, creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            deadline = time.monotonic() + 20
            while True:
                if child.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError(f"Packaged media helper did not start. Inspect {log}.")
                try:
                    health = request("/lummix-health")
                    break
                except requests.ConnectionError:
                    time.sleep(.2)
            assert health["service"] == "lummix-media" and MEDIA_CAPABILITIES <= set(health["capabilities"])
            settings = request("/mix/preferences")
            mix = request("/mix/gains")
            if phase == "seed":
                settings = request("/mix/preferences", token=settings["token"], patch={"settings": TEST_PREFERENCES})
                mix = request("/mix/gains", token=mix["token"], patch=TEST_GAINS)
            assert settings["settings"] == TEST_PREFERENCES, (phase, settings["settings"])
            assert {key: mix[key] for key in TEST_GAINS} == TEST_GAINS and mix["routeActive"] is False
        finally:
            session.close()
            child.stdin.close()
            try:
                child.wait(timeout=12)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
                raise
    assert child.returncode == 0
    assert json.loads(preferences.read_bytes()) == {"settings": TEST_PREFERENCES}
    sections = json.loads(backup.read_bytes())["settings"]
    assert all(sections[path.name] == json.loads(path.read_bytes()) for path in (preferences, gains))
    report = {"phase": phase, "helperPid": child.pid, "routeActive": mix["routeActive"],
              "helperExitedCleanly": True, "restoredPrimaryFromBackup": phase == "recover",
              "preferences": TEST_PREFERENCES, "gains": TEST_GAINS,
              "settingsHashes": {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in files}}
    (EVIDENCE / f"profile-{phase}.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))


def main(installed_exe=None):
    report = {}
    with tempfile.TemporaryDirectory(prefix="LumMixPortableTest-") as folder:
        if installed_exe is None:
            root = Path(folder)/"Unzipped app"
            shutil.copytree(HERE.parents[1]/".build/frozen/runtime", root)
            exe = str((root/"Lummix Helper.exe").rename(root/"Lummix.exe"))
            (root/"studio").mkdir()
            for name in STUDIO_FILES:
                shutil.copy2(UI/unquote(name),root/"studio"/unquote(name))
        else:
            exe = str(installed_exe.resolve(strict=True))
        image = Path(exe).read_bytes()
        pe_offset, = struct.unpack_from("<I", image, 0x3c)
        assert image[pe_offset:pe_offset+4] == b"PE\0\0"
        subsystem, = struct.unpack_from("<H", image, pe_offset + 24 + 68)
        assert subsystem == 2, "Explorer would allocate a console before the mixer opens"
        report["windowsGuiSubsystem"] = True
        cwd = Path(folder)/"Unrelated working directory"
        cwd.mkdir()
        log = EVIDENCE/"private-runtime-test.log"
        system = Path(os.environ["SystemRoot"])
        env = os.environ.copy()
        env.update(PATH=os.pathsep.join(map(str,(system/"System32",system))),
                   PYTHONNOUSERSITE="1", LUMMIX_QA_LOOPBACK="2", LOCALAPPDATA=str(cwd/"local"))
        for var in ("PYTHONHOME","PYTHONPATH","VIRTUAL_ENV"):
            env.pop(var,None)
        assert shutil.which("python", path=env["PATH"]) is None
        proc = {}
        s = requests.Session()
        s.trust_env = False
        def fetch(port, route, origin=True, **kwargs):
            headers = {"Host":f"127.0.0.1:{port}"}
            if origin:
                headers["Origin"] = "http://127.0.0.1:41835"
            return s.get(f"http://127.0.0.2:{port}"+route,headers=headers | kwargs.pop("headers",{}),
                         timeout=kwargs.pop("timeout",12), allow_redirects=False, **kwargs)
        with log.open("wb") as out:
            try:
                for port, mode in ((41835,"studio"),(41836,"wrist"),(41838,"spotify"),(41839,"addon")):
                    proc[port] = subprocess.Popen([exe,mode,"--parent-stdin"],cwd=cwd,env=env,
                        creationflags=subprocess.CREATE_NO_WINDOW,stdin=subprocess.PIPE,stdout=out,stderr=out)
                for port in proc:
                    route = {41838:"/mix/gains", 41839:"/lummix-addon"}.get(port, "/lummix-health")
                    for _ in range(80):
                        if proc[port].poll() is not None:
                            raise RuntimeError(f"{port} exited; inspect {log}")
                        try:
                            if fetch(port,route).status_code == 200:
                                break
                        except requests.ConnectionError:
                            pass
                        time.sleep(.15)
                    else:
                        raise RuntimeError(f"{port} not ready")
                report["studioAssetsWithDifferentLineEndings"] = verify_studio_assets(
                    fetch, Path(exe).parent / "studio"
                )
                native_studio = Path(exe).with_name("Lummix Host.exe")
                assert any(Path(child.exe()) == native_studio for child in
                           psutil.Process(proc[41835].pid).children()), "Packaged Studio did not run the Rust host"
                report["nativeStudioSha256"] = hashlib.sha256(native_studio.read_bytes()).hexdigest()
                assert fetch(41835,"/private-runtime-test.log").status_code == 404
                report["studioAssetsExact"] = len(STUDIO_FILES)
                report["wrist"] = fetch(41836,"/lummix-health").json()
                cable_state = fetch(41836,"/setup-status",timeout=35).json()["state"]
                if cable_state == "unavailable":
                    # A cold Windows CIM provider can time out on its first enumeration.
                    # Wait for the same eight-second cache expiry that the app uses.
                    time.sleep(8.2)
                    cable_state = fetch(41836,"/setup-status",timeout=35).json()["state"]
                assert cable_state in ("ready","missing","partial"), cable_state
                assert fetch(41836,"/preview").status_code in (200,302)
                mix = fetch(41838,"/mix/gains").json()
                dll = Path(exe).parent / "_internal/lummix_dsp.dll"
                assert dll.is_file() and any(Path(mapping.path) == dll for mapping in
                    psutil.Process(proc[41838].pid).memory_maps()), "Installed media service did not load its Rust mixer"
                report["loadedRustMixer"] = {"bytes":dll.stat().st_size, "sha256":hashlib.sha256(dll.read_bytes()).hexdigest()}
                assert mix["engineAvailable"] and not mix["routeActive"]
                report["nativeMix"] = {k:mix[k] for k in ("engineAvailable","micGain","musicGain","routeActive")}
                panel = fetch(41839, "/wrist")
                assert panel.status_code == 200 and 'id="gain"' in panel.text
                addon = fetch(41839, "/lummix-addon").json()
                assert "canInstall" in addon and "canRemove" in addon
                assert fetch(41839, "/lummix-addon", origin=False).status_code == 403
                assert fetch(41839, "/controls", origin=False).status_code == 403
                panel_headers = {"X-Lummix-Panel":"1", "Origin":"http://127.0.0.1:41839"}
                assert fetch(41839, "/step", headers=panel_headers).json()["step"] in (1,5,10,20)
                assert fetch(41839, "/controls", headers=panel_headers).json()["musicGain"] == mix["musicGain"]
                changed = s.post("http://127.0.0.2:41839/controls", headers={"Host":"127.0.0.1:41839", **panel_headers},
                                 json={"musicGain":150,"micMuted":True,"musicMuted":True}, timeout=5)
                changed.raise_for_status()
                actual = fetch(41838, "/mix/gains").json()
                assert actual["musicGain"] == 150 and actual["micMuted"] and actual["musicMuted"] and not actual["routeActive"]
                report["wristControls"] = {"addonState":addon["state"], "realIsolatedGain":150,"bothMutesApplied":True}
                health=fetch(41838,"/lummix-health").json()
                assert health["service"]=="lummix-media" and health["protocol"]==2
                assert MEDIA_CAPABILITIES<=set(health["capabilities"])
                media_dll = Path(exe).parent / "_internal/lummix_media.dll"
                assert "spotify-smtc-rust-v1" in health["capabilities"]
                assert media_dll.is_file() and any(Path(mapping.path) == media_dll for mapping in
                    psutil.Process(proc[41838].pid).memory_maps()), "Packaged Spotify service did not load Rust media"
                report["loadedRustMedia"] = {
                    "bytes": media_dll.stat().st_size, "sha256": hashlib.sha256(media_dll.read_bytes()).hexdigest()
                }
                report["mediaCapabilities"]=sorted(health["capabilities"])
                mic=fetch(41838,"/mix/mic").json()
                assert isinstance(mic["inputs"],list) and mic["routeActive"] is False
                assert mic["activeId"] is None and mic["latencyMs"] is None and mic["micRms"] is None
                report["readOnlyMic"]={"inputCount":len(mic["inputs"]),"routeActive":False,"cableReady":mic["cableReady"]}
                assert mic["spotifyCapture"]=="off"
                report["readOnlyMic"]["spotifyCapture"]=mic["spotifyCapture"]
                report["readOnlyMic"]["captureAlias"]=mic["captureAlias"]
                if mic["captureAlias"]=="ready":
                    result=subprocess.run([exe,"name-cable"],cwd=cwd,env=env,capture_output=True,
                        creationflags=subprocess.CREATE_NO_WINDOW,timeout=20,check=True)
                    alias=json.loads(result.stdout)
                    assert alias["aliasError"] is None and alias["captureName"]==mic["captureName"]
                    report["alreadyNamedEndpoint"]=alias["captureName"]
                player = fetch(41838,"/spotify/status").json()
                report["spotify"] = {k:player[k] for k in ("state","edition","installationKnown")}
                assert player["state"] in {"ready","detected","unsupported","unavailable","no_session","ambiguous"}
                if player.get("artwork"):
                    from PIL import Image
                    encoded = player["artwork"].partition(",")[2]
                    with Image.open(io.BytesIO(base64.b64decode(encoded))) as img:
                        img.load()
                        report["spotifyArtworkPixels"] = list(img.size)
                if player["state"]=="ready":
                    peak=fetch(41838,"/spotify/peak",headers={"X-Lum-Spotify-Token":player["token"]}).json()
                    assert peak["state"] in {"ready","paused","no_local_signal"}
                    if peak["state"]=="ready": assert 0 <= peak["peak"] <= 1
                    report["personalPeakRead"]=peak
                assert fetch(41838,"/spotify/volume").status_code == 403
                assert fetch(41835,"/spotify-volume.js").status_code == 404
                report["retiredPlayerVolumeAssetServed"] = False
                ducks = fetch(41838,"/mix/duck").json()
                assert ducks["state"] in ("waiting_mic","off","unavailable")
                report["duck"] = {k:ducks[k] for k in ("enabled","state","depthDb")}
                settings=fetch(41838,"/mix/preferences").json()
                assert settings["settings"].get("closeToTray",True) is True
                report["freshProfileCloseToTray"]=True
                assert fetch(41838,"/mix/gains",headers={"Origin":"http://untrusted.invalid"}).status_code == 403
                check = subprocess.run([exe,"verify-vendor"],cwd=cwd,env=env,stdout=out,stderr=out,
                                       creationflags=subprocess.CREATE_NO_WINDOW,timeout=50)
                assert check.returncode == 0, "vendor verification failed"
                report["vendorVerifiedNoInstall"] = True
            finally:
                s.close()
                for child in proc.values():
                    child.stdin.close()
                for child in proc.values():
                    try:
                        child.wait(timeout=12)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait()
        assert all(p.returncode==0 for p in proc.values())
        report.update(gracefulServiceExits=len(proc), pythonOnPath=False,
                    helperSha256=hashlib.sha256(Path(exe).read_bytes()).hexdigest())
    print(json.dumps(report,indent=2))
    (EVIDENCE/"verified-runtime.json").write_text(json.dumps(report, indent=2)+"\n",encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--installed-exe", type=Path, help="Test the installed executable and its adjacent files in place.")
    parser.add_argument("--profile-phase", choices=("seed", "preserved", "recover"), help="Hosted reinstall QA phase.")
    options = parser.parse_args()
    if options.profile_phase:
        if options.installed_exe is None:
            parser.error("--profile-phase requires --installed-exe")
        verify_reinstall_profile(options.installed_exe.resolve(strict=True), options.profile_phase)
    else:
        main(options.installed_exe)
