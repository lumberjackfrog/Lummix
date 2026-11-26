"""Build a self-contained x64 Windows directory for the desktop launcher."""
import hashlib
import os
from importlib import metadata
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import urllib.request
import zipfile

HERE = Path(__file__).resolve().parent
SRC = HERE.parent
BUILD = SRC.parent / ".build"
DIST = BUILD / "frozen"
DEST = DIST / "runtime"
sys.path.insert(0,str(SRC))
import cable_setup
from native_build import build_all, copy_rust_notices


def webview_sdk():
    version = "1.0.4370.26"
    pin = "1c0dacac84b70109b3fd42f90c53a0ba942abc3a87590d7a4e0ccd86e4a06cf4"
    archive = BUILD/f"Microsoft.Web.WebView2.{version}.nupkg"
    if not archive.exists():
        url = f"https://api.nuget.org/v3-flatcontainer/microsoft.web.webview2/{version}/microsoft.web.webview2.{version}.nupkg"
        with urllib.request.urlopen(url, timeout=60) as response:
            data = response.read()
        if hashlib.sha256(data).hexdigest() != pin:
            raise RuntimeError("Unexpected Microsoft WebView2 SDK download")
        archive.write_bytes(data)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != pin:
        raise RuntimeError("Microsoft WebView2 SDK package hash mismatch")
    folder = BUILD/"webview2"
    folder.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive) as package:
        for name in ("WebView2.tlb", "runtimes/win-x64/native/WebView2Loader.dll", "LICENSE.txt", "NOTICE.txt"):
            (folder/Path(name).name).write_bytes(package.read(name))
    loader=folder/"WebView2Loader.dll"
    if hashlib.sha256(loader.read_bytes()).hexdigest() != "37996d81f0f3a13e93549f74f722fce25d70b00e93e4a470c34b75d7231bfd1d":
        raise RuntimeError("Unexpected x64 Microsoft WebView2 loader")
    ps=Path(os.environ["SystemRoot"])/"System32/WindowsPowerShell/v1.0/powershell.exe"
    cmd="$s=Get-AuthenticodeSignature -LiteralPath $env:LUMMIX_WEBVIEW_LOADER; if($s.Status -ne 'Valid' -or $s.SignerCertificate.GetNameInfo('SimpleName',$false) -ne 'Microsoft Corporation'){exit 1}"
    subprocess.run([str(ps),"-NoProfile","-NonInteractive","-Command",cmd],check=True,
                   env={**os.environ,"PSModulePath":str(ps.parent/"Modules"),"LUMMIX_WEBVIEW_LOADER":str(loader)})
    return folder


def write_notices(webview):
    notices = DEST / "licenses"
    notices.mkdir(exist_ok=True)
    copy_rust_notices(notices)
    shutil.copy2(SRC/"packaging/PortAudio-LICENSE.txt",notices/"PortAudio.txt")
    shutil.copy2(SRC/"packaging/Rust-LICENSE-MIT.txt",notices/"Rust.txt")
    (notices/"WebView2").mkdir(exist_ok=True)
    for name in ("LICENSE.txt","NOTICE.txt"):
        shutil.copy2(webview/name,notices/"WebView2"/name)
    shutil.copy2(Path(sys.base_prefix)/"LICENSE.txt", notices/"Python.txt")
    # Retain wheel-distributed notices for direct and transitive runtime components.
    packages = ("numpy","pycaw","comtypes","psutil","sounddevice","cffi","pycparser",
                "pillow","aiohttp","aiohappyeyeballs","aiosignal","attrs","frozenlist","multidict",
                "propcache","yarl","idna","packaging","typing_extensions","pyinstaller","pywin32","charset-normalizer")
    lines = ["Bundled Windows x64 runtime", "Python " + platform.python_version()]
    for package in packages:
        d = metadata.distribution(package)
        lines.append(f"{d.metadata['Name']} {d.version}")
        for f in d.files or ():
            if any(x in f.name.upper() for x in ("LICENSE","LICENCE","COPYING","NOTICE")) and f.suffix != ".py":
                p = Path(d.locate_file(f))
                if p.is_file():
                    target = notices/package/Path(*(part for part in f.parts if part != ".."))
                    target.parent.mkdir(parents=True,exist_ok=True)
                    shutil.copy2(p,target)
    (notices/"COMPONENTS.txt").write_text("\n".join(lines)+"\n",encoding="utf-8")


def main():
    if platform.system() != "Windows" or platform.machine().upper() != "AMD64":
        raise RuntimeError("Use an x64 Windows environment to build this runtime")
    BUILD.mkdir(parents=True, exist_ok=True)
    native = build_all()
    os.environ["LUMMIX_HOST_DLL"] = str(native["host_lib"])
    archive = BUILD / "VBCABLE_Driver_Pack46.zip"
    if not archive.exists():
        archive.write_bytes(cable_setup.download())
    with cable_setup.package(archive.read_bytes):
        pass
    args = [sys.executable,"-m","PyInstaller","--noconfirm","--onedir","--windowed","--noupx",
            "--additional-hooks-dir",str(HERE/"hooks"),"--name","Lummix Helper","--icon",str(SRC/"packaging/Lummix.ico"),"--paths",str(SRC),"--distpath",str(DIST),
            "--workpath",str(BUILD/"analysis"),"--specpath",str(BUILD)]
    # Package the system UIA typelib wrappers so portable installs can run read-only.
    from comtypes.client import GetModule
    uia = GetModule("UIAutomationCore.dll")
    webview = webview_sdk()
    wv = GetModule(str(webview/"WebView2.tlb"))
    for name in ("dsp", "media", "host_lib"):
        args += ["--add-binary", str(native[name])+";."]
    args += ["--add-binary", str(webview/"WebView2Loader.dll")+";.", "--add-data", str(SRC/"packaging/Lummix.ico")+";."]
    for module in (wv.__name__, wv.__wrapper_module__.__name__, uia.__name__, uia.__wrapper_module__.__name__, "wrist-server","spotify-helper","detect_openvr"):
        args += ["--hidden-import",module]
    # Optional third-party integrations of Python packages, unused in this application.
    for module in ("matplotlib","scipy","torch","tensorflow","pandas","IPython","jupyter","pytest",
                   "tkinter","PySide6","PyQt6","cv2","openpyxl","sklearn","sphinx","cryptography",
                   "av","soundfile","numba","onnx","fsspec","Cython"):
        args += ["--exclude-module",module]
    for path in (SRC/"wrist-tile.html",SRC/"wrist-fixture.js",SRC/"cable_setup_install.ps1",
                 SRC/"cable_setup_status.ps1",SRC/"xsoverlay-icon.png",SRC/"xsoverlay-live.js",
                 SRC/"xsoverlay-panel.html",HERE/"studio-assets.txt",SRC.parent/"studio/controls-bridge.js",
                 native["controls"],archive):
        args += ["--add-data",str(path)+";."]
    args += [str(HERE/"runtime_main.py")]
    subprocess.run(args,check=True)
    built = DIST / "Lummix Helper"
    # Publish into our own staging directory; retain the previous tree until replacement succeeds.
    if DEST.exists():
        if DEST.resolve().parent != DIST.resolve():
            raise RuntimeError("Unexpected destination")
        shutil.rmtree(DEST)
    built.rename(DEST)
    shutil.copy2(native["host"],DEST/"Lummix Host.exe")
    write_notices(webview)
    print("Packaged",DEST,hashlib.sha256((DEST/"Lummix Helper.exe").read_bytes()).hexdigest())


if __name__ == "__main__":
    main()
