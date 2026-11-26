"""Build the portable archive from the exact staged Rust runtime."""
from hashlib import sha256
from pathlib import Path
import sys
from urllib.parse import unquote
import zipfile


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
RUNTIME = HERE.parent / "runtime"
STUDIO = ROOT / "studio"
sys.path.insert(0, str(RUNTIME))
from studio_assets import STUDIO_FILES


def assemble(destination=ROOT / "dist", runtime=ROOT / ".build/frozen/runtime"):
    launcher = runtime / "Lummix.exe"
    if not launcher.is_file():
        raise RuntimeError("Stage the native Windows runtime before packaging")
    files = [path for path in sorted(runtime.rglob("*")) if path.is_file()]
    if any(path.suffix.lower() in {".py", ".pyc", ".pyd"}
           or path.name.lower().startswith("python") and path.suffix.lower() == ".dll"
           or path.name in {"Lummix Helper.exe", "Lummix Host.exe", "lummix_dsp.dll", "lummix_media.dll", "lummix_host.dll"}
           for path in files):
        raise RuntimeError("The native stage contains a legacy runtime artifact")
    name = Path(destination) / "Lummix for Windows.zip"
    name.parent.mkdir(parents=True, exist_ok=True)
    tmp = name.with_suffix(".zip.tmp")
    with zipfile.ZipFile(tmp, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=5) as archive:
        archive.write(HERE / "Read me.txt", "Lummix/Read me.txt")
        for path in files:
            archive.write(path, "Lummix/" + path.relative_to(runtime).as_posix())
    with zipfile.ZipFile(tmp) as archive:
        if archive.testzip() is not None:
            raise RuntimeError("The packaged ZIP did not pass verification")
        for entry in STUDIO_FILES:
            file = unquote(entry)
            source = runtime / "_internal/controls.wasm" if file == "controls.wasm" else STUDIO / file
            if archive.read("Lummix/studio/" + file) != source.read_bytes():
                raise RuntimeError(f"Packaged Studio asset differs from source: {file}")
        if archive.read("Lummix/Lummix.exe") != launcher.read_bytes():
            raise RuntimeError("Packaged executable differs from the native stage")
    tmp.replace(name)
    print(name, name.stat().st_size, sha256(name.read_bytes()).hexdigest())
    return name


if __name__ == "__main__":
    assemble()
