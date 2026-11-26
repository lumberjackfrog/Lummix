"""Compare the actual two installer payloads on the same disposable Windows runner."""
import argparse
import hashlib
import http.client
import json
import math
import msilib
import os
from pathlib import Path
import shutil
import socket
import statistics
import subprocess
import tempfile
import time

import psutil
from bench_desktop import compare_desktop

BASELINE = "9cee51354fc8ec037da060e3926ecc71f147750c683b2dfe9dd100b7a1264079"
ORIGIN = "http://127.0.0.1:41835"
HOST = "127.0.0.1:41838"
BIND = "127.0.0.2"


def extract_installer(installer, destination, expected):
    actual = hashlib.sha256(installer.read_bytes()).hexdigest()
    if actual != expected:
        raise ValueError(f"Installer checksum mismatch for {installer.name}")
    seven = shutil.which("7z") or r"C:\Program Files\7-Zip\7z.exe"
    with tempfile.TemporaryDirectory(dir=destination.parent) as temp:
        tmp = Path(temp)
        for payload, selector in ((installer, "-ir!Lummix.msi"), (tmp / "Lummix.msi", "-ir!f*")):
            subprocess.run([seven, "e", "-aoa", "-bso0", f"-o{tmp}", selector, "--", payload], check=True)
        db = msilib.OpenDatabase(str(tmp / "Lummix.msi"), msilib.MSIDBOPEN_READONLY)

        def rows(query, fields):
            view = db.OpenView(query)
            view.Execute(None)
            result = []
            while record := view.Fetch():
                result.append(tuple(record.GetString(n+1) for n in range(fields)))
            view.Close()
            return result

        dirs = {k: (parent, name.split("|")[-1]) for k,parent,name in
                rows("SELECT `Directory`,`Directory_Parent`,`DefaultDir` FROM `Directory`",3)}
        components = dict(rows("SELECT `Component`,`Directory_` FROM `Component`",2))
        for identity,comp,name in rows("SELECT `File`,`Component_`,`FileName` FROM `File`",3):
            parts = [name.split("|")[-1]]
            directory = components[comp]
            while directory != "VERSIONDIR":
                directory,part = dirs[directory]
                parts.append(part)
            relative = Path(*reversed(parts))
            if relative.is_absolute() or ".." in relative.parts:
                raise ValueError("Unexpected installer file path")
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(tmp / identity, target)
        db.Close()
    return (destination / "Lummix.exe").resolve(strict=True)


def request(path, connecting=False):
    conn = http.client.HTTPConnection(BIND, 41838, timeout=2)
    try:
        if connecting:
            conn.timeout = .012
            conn.connect()
            conn.sock.settimeout(2)
        conn.request("GET", path, headers={"Host": HOST, "Origin": ORIGIN})
        response = conn.getresponse()
        data = response.read()
        if response.status != 200:
            raise ValueError((path,response.status,data[:256]))
        return json.loads(data)
    finally:
        conn.close()


def measure_media(exe, profile):
    with socket.socket() as check:
        check.bind((BIND,41838))
    profile.mkdir()
    environment = dict(os.environ, LOCALAPPDATA=str(profile), LUMMIX_QA_LOOPBACK="2")
    started = time.perf_counter()
    with subprocess.Popen([exe, "spotify", "--parent-stdin"], cwd=exe.parent, env=environment,
                          stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                          creationflags=subprocess.CREATE_NO_WINDOW) as worker:
        try:
            while True:
                if worker.poll() is not None:
                    raise AssertionError(("Media worker exited",worker.returncode))
                try:
                    health = request("/lummix-health", connecting=True)
                    break
                except (OSError,http.client.HTTPException):
                    if time.perf_counter()-started>30:
                        raise
                    time.sleep(.002)
            ready = 1000*(time.perf_counter()-started)
            assert health["service"] == "lummix-media" and health["protocol"] == 2
            process = psutil.Process(worker.pid)
            assert any(c.laddr == (BIND,41838) and c.status=="LISTEN" for c in process.net_connections("tcp"))
            latencies = []
            for _ in range(80):
                before = time.perf_counter_ns()
                gain = request("/mix/gains")
                latencies.append((time.perf_counter_ns()-before)/1e6)
                assert gain["micGain"] == gain["musicGain"] == 100 and gain["routeActive"] is False
            before = time.perf_counter()
            spotify = request("/spotify/status")
            status_ms = (time.perf_counter()-before)*1000
            assert isinstance(spotify.get("state"), str) and "playing" in spotify and "canPlay" in spotify, spotify
            family = [process, *process.children(recursive=True)]
            mem = [p.memory_info() for p in family]
            result = {"ready_ms": ready,"gains_median_ms": statistics.median(latencies),
                      "gains_p95_ms":sorted(latencies)[math.ceil(.95*len(latencies))-1],
                      "status_first_ms":status_ms,"status_state":spotify["state"],
                      "working_set_mib":sum(m.rss for m in mem)/2**20,
                      "private_mib":sum(m.private for m in mem)/2**20,"processes":len(mem)}
        finally:
            worker.stdin.close()
            try:
                worker.wait(15)
            except subprocess.TimeoutExpired:
                worker.kill()
                raise
        if worker.returncode:
            raise AssertionError(("Shutdown failed",worker.returncode))
    return result


def measure_setup(installer, exe):
    """Time the real silent one-click installer and the signed-payload check separately."""
    import ctypes
    # The Installer service may publish the product in another registry hive; use
    # the package's actual ProductCode and the Windows Installer product database.
    with tempfile.TemporaryDirectory() as temp:
        seven = shutil.which("7z") or r"C:\Program Files\7-Zip\7z.exe"
        subprocess.run([seven,"e","-aoa","-bso0",f"-o{temp}","-ir!Lummix.msi","--",installer],check=True)
        db=msilib.OpenDatabase(str(Path(temp)/"Lummix.msi"),msilib.MSIDBOPEN_READONLY)
        view=db.OpenView("SELECT `Value` FROM `Property` WHERE `Property`='ProductCode'")
        view.Execute(None)
        product=view.Fetch().GetString(1)
        view.Close()
        view=db.OpenView("SELECT `Value` FROM `Property` WHERE `Property`='ProductVersion'")
        view.Execute(None)
        version=view.Fetch().GetString(1)
        view.Close()
        db.Close()
    query=ctypes.windll.msi.MsiQueryProductStateW
    query.argtypes=[ctypes.c_wchar_p]
    query.restype=ctypes.c_int
    assert query(product)<1, "Expected an uninstalled product on the disposable runner"
    try:
        begin=time.perf_counter()
        subprocess.run([installer,"/S"],timeout=120,check=True)
        install_ms=1000*(time.perf_counter()-begin)
        assert query(product)==5,"Windows Installer did not register the product"
        installed = Path(os.environ["LOCALAPPDATA"]) / "Programs/Lummix" / version / "Lummix.exe"
        assert hashlib.sha256(installed.read_bytes()).digest() == hashlib.sha256(exe.read_bytes()).digest()
        begin=time.perf_counter()
        result=subprocess.run([installed,"verify-vendor"],timeout=90,check=True,capture_output=True)
        assert b"verif" in result.stdout.lower(),result.stdout
        verify_ms=1000*(time.perf_counter()-begin)
    finally:
        if query(product)>0:
            subprocess.run(["msiexec.exe","/x",product,"/qn","/norestart"],timeout=120,check=True)
        assert query(product)<1
    return {"install_ms":install_ms,"driver_verification_ms":verify_ms}

def main(args):
    if os.environ.get("GITHUB_ACTIONS") != "true":
        raise RuntimeError("Use the disposable Windows Actions runner for desktop and setup comparisons.")
    args.output.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as work:
        work=Path(work)
        old=extract_installer(args.before,work/"before",BASELINE)
        new=extract_installer(args.after,work/"after",args.after_hash)
        measurements={k:[] for k in ("before","after")}
        # Alternating paired trials compensate for machine warm-up and order.
        for i in range(6):
            trials=(("before",old),("after",new))
            for name,exe in (trials if i%2==0 else reversed(trials)):
                result=measure_media(exe,work/f"profile-{name}-{i}")
                measurements[name].append(result)
        medians={k:{metric:statistics.median(row[metric] for row in group)
                    for metric in group[0] if metric!="status_state"} for k,group in measurements.items()}
        report={"pairs":6,"baseline_sha256":BASELINE,"candidate_sha256":args.after_hash,
                "installer_bytes":{"before":args.before.stat().st_size,"after":args.after.stat().st_size},
                "media_trials":measurements,"media_medians":medians}
        args.output.write_text(json.dumps(report,indent=2),encoding="utf-8")
        setup={"before":[],"after":[]}
        pairs=(("before",args.before,old),("after",args.after,new))
        for i in range(4):
            for tag,installer,exe in (pairs if i%2==0 else reversed(pairs)):
                setup[tag].append(measure_setup(installer.resolve(),exe))
        report["setup_trials"]=setup
        report["setup_medians"]={tag:{key:statistics.median(row[key] for row in rows)
                                      for key in rows[0]} for tag,rows in setup.items()}
        args.output.write_text(json.dumps(report,indent=2),encoding="utf-8")
        report["desktop"]=compare_desktop(old,new,work)
        args.output.write_text(json.dumps(report,indent=2),encoding="utf-8")
        print(json.dumps(medians,indent=2),flush=True)

if __name__=="__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    for key in ("before","after","output"):
        parser.add_argument("--"+key,type=Path,required=True)
    parser.add_argument("--after-hash",required=True)
    main(parser.parse_args())
