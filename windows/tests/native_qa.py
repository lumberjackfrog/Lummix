"""Checks for the Windows release image from the external Python QA harness."""
from pathlib import Path
import re

import psutil

WINDOWS = Path(__file__).resolve().parents[1]
STAGED = WINDOWS.parent / ".build/frozen/runtime/Lummix.exe"
STUDIO_FILES = frozenset((WINDOWS / "runtime/studio-assets.txt").read_text(encoding="ascii").split())
LEGACY_IMAGES = {"lummix helper.exe", "lummix host.exe", "lummix_dsp.dll",
                 "lummix_media.dll", "lummix_host.dll"}


def is_legacy(path):
    name = path.name.casefold()
    return (name in LEGACY_IMAGES or path.suffix.casefold() in {".py", ".pyc", ".pyd"}
            or re.fullmatch(r"python.*\.(dll|exe|zip)", name) is not None)


def verify_bundle(exe):
    assert exe.name == "Lummix.exe" and exe.is_file(), exe
    for filename in ("WebView2Loader.dll", "libportaudio64bit.dll", "controls.wasm"):
        assert (exe.parent / "_internal" / filename).is_file(), filename
    stale = [path for path in exe.parent.rglob("*") if path.is_file() and is_legacy(path)]
    assert not stale, f"Legacy runtime artifacts in {exe.parent}: {stale}"


def verify_process(exe, pid, *, modules=()):
    process = psutil.Process(pid)
    assert Path(process.exe()).samefile(exe), (pid, process.exe(), exe)
    maps = [Path(mapping.path) for mapping in process.memory_maps()]
    assert not any(is_legacy(path) for path in maps), maps
    mapped = {str(path).casefold() for path in maps}
    for module in modules:
        assert str((exe.parent / "_internal" / module).resolve()).casefold() in mapped, (pid, module)
    for child in process.children(recursive=True):
        try:
            assert not is_legacy(Path(child.exe())), child.exe()
        except psutil.NoSuchProcess:
            pass
    return process


def verify_listener(exe, pid, address, port):
    process = verify_process(exe, pid)
    assert any(connection.status == psutil.CONN_LISTEN and connection.laddr == (address, port)
               for connection in process.net_connections(kind="tcp")), (pid, address, port)
