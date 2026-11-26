"""Stage the x64 Rust desktop executable and its verified Windows resources."""
import ast
import hashlib
import json
import os
from importlib import metadata
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from urllib.parse import unquote
import zipfile

HERE = Path(__file__).resolve().parent
SRC = HERE.parent
BUILD = SRC.parent / ".build"
DIST = BUILD / "frozen"
DEST = DIST / "runtime"
STUDIO = SRC.parent / "studio"
sys.path.insert(0, str(SRC))
from native_build import build_all, copy_rust_notices

SDK_VERSION = "1.0.4370.26"
SDK_HASH = "1c0dacac84b70109b3fd42f90c53a0ba942abc3a87590d7a4e0ccd86e4a06cf4"
LOADER_HASH = "37996d81f0f3a13e93549f74f722fce25d70b00e93e4a470c34b75d7231bfd1d"
PORTAUDIO_HASH = "ec080194f01e4095c7fb43dbd7ed05af922c5b34295056a9ff56782741d65481"
CABLE_ARCHIVE = "VBCABLE_Driver_Pack46.zip"
CABLE_SIZE = 1_324_680
CABLE_HASH = "bc44e8974191a5a2427d6c979e2dab08dbb09925f3f435dfa2f0ba3e45b6eb90"
CABLE_URL = f"https://download.vb-audio.com/Download_CABLE/{CABLE_ARCHIVE}"

RESOURCE_NAMES = (
    "wrist-tile.html", "wrist-fixture.js", "cable_setup_install.ps1", "cable_setup_status.ps1",
    "xsoverlay-icon.png", "xsoverlay-live.js", "xsoverlay-panel.html",
)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None


def digest(data):
    return hashlib.sha256(data).hexdigest()


def pinned_download(path, url, expected_hash, limit, expected_size=None, *, follow_redirects=False):
    if path.exists():
        with path.open("rb") as stream:
            data = stream.read(limit + 1)
    else:
        opener = urllib.request.build_opener() if follow_redirects else urllib.request.build_opener(NoRedirect())
        with opener.open(url, timeout=60) as response:
            if response.status != 200 or not follow_redirects and response.geturl() != url:
                raise RuntimeError(f"Unexpected download response for {path.name}")
            data = response.read(limit + 1)
    if len(data) > limit or (expected_size is not None and len(data) != expected_size):
        raise RuntimeError(f"Unexpected download size for {path.name}")
    if digest(data) != expected_hash:
        raise RuntimeError(f"Unexpected hash for {path.name}")
    if not path.exists():
        with path.open("xb") as stream:
            stream.write(data)
    return data


def system_powershell():
    return Path(os.environ["SystemRoot"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"


def webview_sdk():
    archive = BUILD / f"Microsoft.Web.WebView2.{SDK_VERSION}.nupkg"
    package_id = "microsoft.web.webview2"
    url = f"https://api.nuget.org/v3-flatcontainer/{package_id}/{SDK_VERSION}/{package_id}.{SDK_VERSION}.nupkg"
    pinned_download(archive, url, SDK_HASH, 32 * 1024 * 1024, follow_redirects=True)
    folder = BUILD / "webview2"
    folder.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive) as sdk:
        for name in ("runtimes/win-x64/native/WebView2Loader.dll", "LICENSE.txt", "NOTICE.txt"):
            (folder / Path(name).name).write_bytes(sdk.read(name))
    loader = folder / "WebView2Loader.dll"
    if digest(loader.read_bytes()) != LOADER_HASH:
        raise RuntimeError("Unexpected x64 Microsoft WebView2 loader")
    powershell = system_powershell()
    script = (
        "$s=Get-AuthenticodeSignature -LiteralPath $env:LUMMIX_WEBVIEW_LOADER; "
        "if($s.Status -ne 'Valid' -or "
        "$s.SignerCertificate.GetNameInfo('SimpleName',$false) -ne 'Microsoft Corporation'){exit 1}"
    )
    subprocess.run(
        [str(powershell), "-NoProfile", "-NonInteractive", "-Command", script],
        check=True, timeout=30,
        env={**os.environ, "PSModulePath": str(powershell.parent / "Modules"), "LUMMIX_WEBVIEW_LOADER": str(loader)},
    )
    return folder


def stage_cable(data):
    archive = BUILD / CABLE_ARCHIVE
    pinned_download(archive, CABLE_URL, CABLE_HASH, CABLE_SIZE, CABLE_SIZE)
    members = (SRC / "host-rs/src/cable_members.in").read_text(encoding="ascii")
    expected = {name: (size, expected_hash)
                for name, size, expected_hash in ast.literal_eval(members.split("\n", 1)[1])}
    folder = data / "VBCABLE"
    folder.mkdir()
    with zipfile.ZipFile(archive) as package:
        entries = package.infolist()
        if len(entries) != len(expected) or {entry.filename for entry in entries} != set(expected):
            raise RuntimeError("Unexpected VB-Audio package layout")
        for entry in entries:
            name = entry.filename
            size, expected_hash = expected[name]
            if Path(name).name != name or entry.is_dir() or entry.file_size != size:
                raise RuntimeError(f"Unexpected VB-Audio package member {name}")
            payload = package.read(entry)
            if len(payload) != size or digest(payload) != expected_hash:
                raise RuntimeError(f"VB-Audio member hash mismatch for {name}")
            with (folder / name).open("xb") as member:
                member.write(payload)
    powershell = system_powershell()
    proof = subprocess.check_output(
        [str(powershell), "-NoProfile", "-NonInteractive", "-File", str(SRC / "cable_setup_install.ps1"),
         "-Folder", str(folder)],
        timeout=30, env={**os.environ, "PSModulePath": str(powershell.parent / "Modules")},
    )
    evidence = json.loads(proof)
    if (evidence.get("installerStatus"), evidence.get("catalogStatus")) != ("Valid", "Valid"):
        raise RuntimeError("VB-Audio installer and driver catalog must pass Windows trust checks")
    shutil.copy2(archive, data / CABLE_ARCHIVE)


def write_notices(destination, webview):
    notices = destination / "licenses"
    notices.mkdir()
    copy_rust_notices(notices)
    shutil.copy2(SRC / "packaging/PortAudio-LICENSE.txt", notices / "PortAudio.txt")
    shutil.copy2(SRC / "packaging/Rust-LICENSE-MIT.txt", notices / "Rust.txt")
    webview_notices = notices / "WebView2"
    webview_notices.mkdir()
    for name in ("LICENSE.txt", "NOTICE.txt"):
        shutil.copy2(webview / name, webview_notices / name)
    (notices / "COMPONENTS.txt").write_text(
        f"Bundled Windows x64 runtime\nLummix Rust desktop executable and WebAssembly controls\n"
        f"Microsoft.Web.WebView2 loader {SDK_VERSION}\nPortAudio x64 WASAPI ({PORTAUDIO_HASH})\n"
        "VB-Audio VB-CABLE Pack46 (original archive and matching extracted members)\n",
        encoding="utf-8",
    )


def stage(destination, native, webview):
    shutil.copy2(native["host"], destination / "Lummix.exe")
    data = destination / "_internal"
    data.mkdir()
    portaudio = metadata.distribution("sounddevice").locate_file(
        "_sounddevice_data/portaudio-binaries/libportaudio64bit.dll"
    )
    if digest(portaudio.read_bytes()) != PORTAUDIO_HASH:
        raise RuntimeError("Unexpected Windows x64 PortAudio library")
    for source in (portaudio, webview / "WebView2Loader.dll", SRC / "packaging/Lummix.ico",
                   HERE / "studio-assets.txt", STUDIO / "controls-bridge.js", native["controls"]):
        shutil.copy2(source, data / source.name)
    for name in RESOURCE_NAMES:
        shutil.copy2(SRC / name, data / name)
    stage_cable(data)
    studio = destination / "studio"
    studio.mkdir()
    for entry in (HERE / "studio-assets.txt").read_text(encoding="ascii").split():
        name = unquote(entry)
        source = native["controls"] if name == "controls.wasm" else STUDIO / name
        shutil.copy2(source, studio / name)
    write_notices(destination, webview)


def main():
    if platform.system() != "Windows" or platform.machine().upper() != "AMD64":
        raise RuntimeError("Use an x64 Windows environment to build this runtime")
    DIST.mkdir(parents=True, exist_ok=True)
    native = build_all()
    webview = webview_sdk()
    with tempfile.TemporaryDirectory(prefix="native-stage-", dir=DIST) as folder:
        stage_root = Path(folder) / "runtime"
        stage_root.mkdir()
        stage(stage_root, native, webview)
        previous = Path(folder) / "previous"
        if DEST.exists():
            if DEST.resolve().parent != DIST.resolve():
                raise RuntimeError("Unexpected destination")
            DEST.rename(previous)
        try:
            stage_root.rename(DEST)
        except OSError:
            if previous.exists():
                previous.rename(DEST)
            raise
    print("Staged", DEST, digest((DEST / "Lummix.exe").read_bytes()))


if __name__ == "__main__":
    main()
