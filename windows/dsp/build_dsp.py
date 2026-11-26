"""Build the x64 audio library once for development checks or freezing."""
from pathlib import Path
import subprocess


def build_dsp():
    crate = Path(__file__).resolve().parent
    target = crate.parents[1] / ".build/dsp"
    try:
        subprocess.run(["cargo", "build", "--release", "--locked", "--offline",
                        "--manifest-path", str(crate / "Cargo.toml"), "--target-dir", str(target)], check=True)
    except FileNotFoundError as exc:
        raise SystemExit("Building Lummix requires the Rust x64 MSVC toolchain (rustc and cargo).") from exc
    return (target / "release/lummix_dsp.dll").resolve(strict=True)


if __name__ == "__main__":
    print(build_dsp())
