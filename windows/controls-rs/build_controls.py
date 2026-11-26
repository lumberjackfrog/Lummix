"""Build the dependency-free controls module shared by Studio and the XSOverlay panel."""
from pathlib import Path
from shutil import copyfile
import subprocess
from tempfile import TemporaryDirectory


def build_controls():
    crate = Path(__file__).resolve().parent
    root = crate.parents[1]
    target = root / ".build" / "controls"
    platform = "wasm32-unknown-unknown"
    subprocess.run(
        ["cargo", "build", "--release", "--locked", "--offline", "--target", platform,
         "--manifest-path", str(crate / "Cargo.toml"), "--target-dir", str(target)],
        check=True,
    )
    output = root / "studio" / "controls.wasm"
    with TemporaryDirectory(dir=target) as staging:
        staged = Path(staging) / output.name
        copyfile(target / platform / "release" / "lummix_controls.wasm", staged)
        staged.replace(output)
    return output


if __name__ == "__main__":
    print(build_controls())
