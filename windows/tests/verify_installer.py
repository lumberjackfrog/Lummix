"""Exercise the exact user MSI, shortcut, repair, native launch and uninstall in private paths."""
import ctypes
from hashlib import sha256
import json
import msilib
from pathlib import Path
import subprocess
import sys
import tempfile
import winreg
import zipfile

from native_qa import verify_bundle

from win32com.client import Dispatch
from win32com.shell import shell, shellcon

HERE=Path(__file__).resolve().parent
EVIDENCE = HERE.parents[1] / ".build/qa"
EVIDENCE.mkdir(parents=True, exist_ok=True)
def main(msi,archive):
    installer_hash=sha256(msi.read_bytes()).hexdigest()
    db=msilib.OpenDatabase(str(msi),msilib.MSIDBOPEN_READONLY)
    view=db.OpenView("SELECT Value FROM Property WHERE Property = 'ProductCode'")
    view.Execute(None)
    product=view.Fetch().GetString(1)
    view.Close()
    db.Close()
    desktop_link=Path(shell.SHGetFolderPath(0,shellcon.CSIDL_DESKTOPDIRECTORY,None,0))/"Lummix.lnk"
    assert not desktop_link.exists(),"A desktop shortcut already exists. Use an isolated user."
    state=ctypes.windll.msi.MsiQueryProductStateW
    assert state(product)<1,"This release is already installed"
    try:winreg.OpenKey(winreg.HKEY_CURRENT_USER,r"Software\Lummix\Installer").Close()
    except FileNotFoundError:pass
    else:raise RuntimeError("A real registered Lummix install is present. Use an isolated user.")
    with tempfile.TemporaryDirectory(prefix="LummixInstallQA-") as folder:
        root=Path(folder)
        target=root/"User programs"
        menu=root/"Start menu"
        def run(op, *args):
            log=EVIDENCE/("last-msi-"+op.lstrip("/")+".log")
            cmd=subprocess.list2cmdline(["msiexec",op,str(msi),"/qn","/norestart","/l*v",str(log)])
            for arg in args:
                key, value=arg.split("=",1)
                assert key in {"INSTALLDIR","SHORTCUTDIR"} and '"' not in value
                cmd+=' '+key+'="'+value+'"'
            subprocess.run(cmd,check=True,timeout=160)
        try:
            run("/i","INSTALLDIR="+str(target),"SHORTCUTDIR="+str(menu))
            assert state(product)==5
            link=menu/"Lummix.lnk"
            shortcut=Dispatch("WScript.Shell").CreateShortCut(str(link))
            exe=Path(shortcut.TargetPath)
            assert exe.is_relative_to(target) and exe.name=="Lummix.exe" and exe.is_file()
            verify_bundle(exe)
            assert desktop_link.is_file() and Path(Dispatch("WScript.Shell").CreateShortcut(str(desktop_link)).TargetPath)==exe
            with zipfile.ZipFile(archive) as z:
                count=0
                for entry in z.infolist():
                    rel=Path(entry.filename).relative_to("Lummix")
                    assert (exe.parent/rel).read_bytes()==z.read(entry),entry.filename
                    count+=1
            # Remove only one expendable, verified, installed QA file and exercise Windows Installer repair.
            readme=exe.parent/"Read me.txt"
            assert readme.is_relative_to(target) and readme.is_file()
            readme.unlink()
            run("/fp")
            assert readme.is_file() and link.is_file()
            subprocess.run([sys.executable,str(HERE/"verify_window.py"),str(exe)],check=True,timeout=120)
            subprocess.run([sys.executable,str(HERE/"verify_tray.py"),str(exe)],check=True,timeout=90)
            report={"payloadFilesExact":count,"msiInstallProductState":5,"version":exe.parent.name,
                    "shortcutPointsToInstalledExe":True,"repairRestoredMissingReadme":True,
                    "installedExeSha256":sha256(exe.read_bytes()).hexdigest(),
                    "msiSha256":installer_hash,
                    "installUi":"quiet installation in disposable per-user paths; actual Windows UI tested from resulting EXE"}
        finally:
            if state(product)>0:run("/x")
        assert state(product)<1 and not link.exists() and not desktop_link.exists() and not exe.exists()
        assert sha256(msi.read_bytes()).hexdigest()==installer_hash
        report["removedAppAndShortcut"]=True
        (EVIDENCE/"verified-installer.json").write_text(json.dumps(report,indent=2)+"\n")
        print(json.dumps(report))

if __name__=="__main__":main(*(Path(arg).resolve() for arg in sys.argv[1:]))
