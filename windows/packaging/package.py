"""Package the shipping frozen helper, own studio assets and the one-click Windows entry."""
from hashlib import sha256
from pathlib import Path
import sys
import zipfile
from urllib.parse import unquote

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[1]
RUNTIME=HERE.parent/"runtime"
STUDIO=ROOT/"studio"
sys.path.insert(0,str(RUNTIME))
from runtime_main import STUDIO_FILES

def assemble(destination=ROOT/"dist"):
    runtime=ROOT/".build/frozen/runtime"
    assert (runtime/"Lummix Helper.exe").is_file()
    name=Path(destination)/"Lummix for Windows.zip"
    name.parent.mkdir(parents=True,exist_ok=True)
    tmp=name.with_suffix(".zip.tmp")
    with zipfile.ZipFile(tmp,"w",compression=zipfile.ZIP_DEFLATED,compresslevel=5) as z:
        z.write(HERE/"Read me.txt","Lummix/Read me.txt")
        for entry in sorted(STUDIO_FILES):
            file=unquote(entry)
            z.write(STUDIO/file,"Lummix/studio/"+file)
        for f in sorted(runtime.rglob("*")):
            if f.is_file():
                dest = "Lummix.exe" if f == runtime/"Lummix Helper.exe" else f.relative_to(runtime).as_posix()
                z.write(f,"Lummix/"+dest)
    with zipfile.ZipFile(tmp) as z:
        assert z.testzip() is None
        for entry in STUDIO_FILES:
            file=unquote(entry)
            assert z.read("Lummix/studio/"+file)==(STUDIO/file).read_bytes()
        assert z.read("Lummix/Lummix.exe")== (runtime/"Lummix Helper.exe").read_bytes()
    tmp.replace(name)
    print(name,name.stat().st_size,sha256(name.read_bytes()).hexdigest())
    return name

if __name__=="__main__": assemble()
