"""Build the x64 application-loopback bridge using the installed Windows SDK and MSVC."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

SOURCE=Path(__file__).resolve().parent/"process_loopback.cpp"

def build(output):
    output=Path(output).resolve()
    installer=Path(os.environ["ProgramFiles(x86)"])/"Microsoft Visual Studio/Installer/vswhere.exe"
    vs=subprocess.check_output([str(installer),"-latest","-products","*","-requires",
                              "Microsoft.VisualStudio.Component.VC.Tools.x86.x64","-property","installationPath"],
                              text=True).strip()
    vc=Path(vs)/"VC/Auxiliary/Build/vcvars64.bat"
    if not vc.is_file():
        raise RuntimeError("MSVC x64 build tools are required.")
    output.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        run=Path(tmp)/"compile.cmd"
        run.write_text(f'@call "{vc}" >nul\n@if errorlevel 1 exit /b 1\n'
                       f'@cl /nologo /std:c++17 /EHsc /O2 /MT /W4 /LD /utf-8 "{SOURCE}" '
                       f'/link /BREPRO /OUT:"{output}" mmdevapi.lib ole32.lib\n',encoding="utf-8")
        subprocess.run([os.environ["COMSPEC"],"/d","/c",str(run)],cwd=tmp,check=True)

if __name__=="__main__":
    build(sys.argv[1])
