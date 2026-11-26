"""Build the pinned first-party native modules for source checks or the Windows installer."""
import json
from pathlib import Path
import runpy
import shutil
import subprocess

WINDOWS = Path(__file__).resolve().parent
ROOT = WINDOWS.parent
CRATES = {"dsp":"dsp", "host-rs":"host", "media_native":"media_native"}


def build_all(test=False):
    subprocess.run(["rustup","target","add","wasm32-unknown-unknown"], check=True)
    for name, directory in CRATES.items():
        manifest = WINDOWS/name/"Cargo.toml"
        common = ["--manifest-path", str(manifest), "--target-dir", str(ROOT/".build"/directory), "--locked"]
        subprocess.run(["cargo","build","--release",*common],check=True)
        if test:
            subprocess.run(["cargo","test",*common],check=True)
    wasm = runpy.run_path(str(WINDOWS/"controls-rs/build_controls.py"))["build_controls"]()
    if test:
        subprocess.run(["cargo","test","--locked","--manifest-path",str(WINDOWS/"controls-rs/Cargo.toml"),
                        "--target-dir",str(ROOT/".build/controls")], check=True)
    release = lambda directory: ROOT/".build"/directory/"release"
    return {name:path.resolve(strict=True) for name,path in {
        "dsp":release("dsp")/"lummix_dsp.dll", "media":release("media_native")/"lummix_media.dll",
        "host":release("host")/"lummix-host-cli.exe", "host_lib":release("host")/"lummix_host.dll",
        "controls":wasm}.items()}


def copy_rust_notices(destination):
    """Bundle the licenses of each locked dependency used by the native components."""
    output = destination/"Rust crates"
    output.mkdir(exist_ok=True)
    copied = set()
    for name in CRATES:
        manifest = WINDOWS/name/"Cargo.toml"
        metadata = subprocess.check_output(["cargo","metadata","--locked","--offline","--format-version","1",
                    "--filter-platform","x86_64-pc-windows-msvc","--manifest-path",str(manifest)],text=True,encoding="utf-8")
        for package in json.loads(metadata)["packages"]:
            if package["source"] is None or package["id"] in copied:
                continue
            copied.add(package["id"])
            source = Path(package["manifest_path"]).parent
            files = [p for p in source.iterdir() if p.is_file() and p.name.upper().startswith(("LICENSE","LICENCE","COPYING","NOTICE"))]
            if not files:
                raise RuntimeError("Missing license file for " + package["id"])
            folder = output/(package["name"]+"-"+package["version"])
            folder.mkdir(exist_ok=True)
            for path in files:
                shutil.copyfile(path,folder/path.name)


if __name__ == "__main__":
    import sys
    build_all("--test" in sys.argv)

