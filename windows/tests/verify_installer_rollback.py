"""Exercise failed installs and upgrades using Windows Installer on a clean hosted runner."""
import ctypes
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from unittest.mock import patch
import uuid
import zipfile

from win32com.client import Dispatch
from win32com.shell import shell, shellcon

WINDOWS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(WINDOWS / "packaging"))
import installer


def package(folder, version, executable):
    archive = folder / f"{version}.zip"
    with zipfile.ZipFile(archive, "w") as files:
        files.writestr("Lummix/Lummix.exe", executable)
        files.write(WINDOWS / "packaging/Lummix.ico", "Lummix/_internal/Lummix.ico")
    target = folder / f"{version}.msi"
    with patch.object(installer, "VERSION", version):
        product = installer.main(archive, target)
    return target, product


def main():
    if os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted" or os.environ.get("GITHUB_ACTIONS") != "true":
        raise RuntimeError("This test installs fixture packages on a disposable GitHub-hosted Windows runner.")
    programs = Path(os.environ["LOCALAPPDATA"]) / "Programs/Lummix"
    links = [
        Path(shell.SHGetFolderPath(0, shellcon.CSIDL_PROGRAMS, None, 0)) / "Lummix/Lummix.lnk",
        Path(shell.SHGetFolderPath(0, shellcon.CSIDL_DESKTOPDIRECTORY, None, 0)) / "Lummix.lnk",
    ]
    assert not programs.exists() and not any(link.exists() for link in links)
    state = ctypes.windll.msi.MsiQueryProductStateW
    state.argtypes = [ctypes.c_wchar_p]
    shortcuts = Dispatch("WScript.Shell")
    logs = WINDOWS.parent / ".build/qa"
    logs.mkdir(parents=True, exist_ok=True)

    def install(operation, target, name, expected=0):
        log = logs / f"rollback-{name}.log"
        result = subprocess.run(["msiexec", operation, str(target), "/qn", "/norestart", "/l*v", str(log)], timeout=120)
        if result.returncode != expected:
            raise AssertionError(f"{name} returned {result.returncode}, expected {expected}. See {log}")
        if expected == 1603:
            assert "RefreshStartup" in log.read_text(encoding="utf-16")

    def check_working(version, product):
        executable = programs / version / "Lummix.exe"
        assert state(product) == 5 and executable.is_file()
        for link in links:
            assert link.is_file() and Path(shortcuts.CreateShortcut(str(link)).TargetPath) == executable
        assert subprocess.check_output([str(executable)], text=True, timeout=10).strip() == "Lummix rollback fixture"

    with tempfile.TemporaryDirectory() as name, patch.object(installer, "FAMILY", uuid.uuid4()):
        folder = Path(name)
        # The two valid packages have a launchable fixture. The broken package fails at process creation.
        source = folder / "Fixture.cs"
        source.write_text('class Fixture { static void Main() { System.Console.WriteLine("Lummix rollback fixture"); } }')
        binary = folder / "Fixture.exe"
        csc = Path(os.environ["WINDIR"]) / "Microsoft.NET/Framework64/v4.0.30319/csc.exe"
        subprocess.run([str(csc), "/nologo", f"/out:{binary}", str(source)], check=True, timeout=30)
        baseline, old_id = package(folder, "7.8.0", binary.read_bytes())
        broken, bad_id = package(folder, "7.8.1", b"Unlaunchable MSI test fixture")
        healthy, good_id = package(folder, "7.8.2", binary.read_bytes())
        try:
            install("/i", broken, "fresh-failure", 1603)
            assert state(bad_id) < 1 and not (programs / "7.8.1").exists()
            assert not any(link.exists() for link in links)
            install("/i", baseline, "baseline")
            check_working("7.8.0", old_id)
            install("/i", broken, "upgrade-failure", 1603)
            assert state(bad_id) < 1 and not (programs / "7.8.1").exists()
            check_working("7.8.0", old_id)
            install("/i", healthy, "retry")
            check_working("7.8.2", good_id)
            assert state(old_id) < 1 and not (programs / "7.8.0").exists()
        finally:
            for product in (old_id, bad_id, good_id):
                if state(product) > 0:
                    install("/x", product, f"remove-{product}")
        assert not any(link.exists() for link in links)
        print("Failed fresh install cleaned up. Failed upgrade restored both launchers. Successful retry upgraded both.")


if __name__ == "__main__":
    main()
