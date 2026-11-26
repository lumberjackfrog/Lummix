"""Exercise the normal launcher startup code against genuine frozen services on a spare loopback."""
from contextlib import ExitStack
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import socket
import sys
import tempfile
import threading
import zipfile
from unittest.mock import patch
from urllib.parse import unquote

HERE=Path(__file__).resolve().parent
EVIDENCE = HERE.parents[1] / ".build/qa"
EVIDENCE.mkdir(parents=True, exist_ok=True)
RUNTIME=HERE.parent/"runtime"
sys.path.insert(0,str(RUNTIME))
import lummix_window as window
from studio_assets import STUDIO_FILES

def run():
    # Relocate sockets only; every HTTP Host, method, required marker, response, and service is genuine.
    real_connect=socket.create_connection
    def connect(address,*args,**kwargs):
        host,port=address
        return real_connect(("127.0.0.2" if host=="127.0.0.1" else host,port),*args,**kwargs)
    with tempfile.TemporaryDirectory() as scratch, ExitStack() as stack:
        chosen=Path(scratch)/"Extracted beside a space"
        if len(sys.argv)>1:
            with zipfile.ZipFile(sys.argv[1]) as z: z.extractall(chosen)
            helper=chosen/"Lummix/Lummix.exe"
        else:
            shutil.copytree(HERE.parents[1]/".build/frozen/runtime",chosen/"runtime")
            (chosen/"studio").mkdir()
            for name in STUDIO_FILES:
                file=unquote(name)
                shutil.copy2(HERE.parents[1]/"studio"/file,chosen/"studio"/file)
            helper=chosen/"runtime/Lummix Helper.exe"
        stack.enter_context(patch.dict(os.environ,LUMMIX_QA_LOOPBACK="2",PATH=os.environ["SystemRoot"]+"\\System32"))
        stack.enter_context(patch("socket.create_connection",connect))
        owned=[]; before={k:window.listening(p) for k,p in window.SERVICES.items()}
        assert not any(before.values()),before
        try:
            for k,p in window.SERVICES.items(): window.ensure(k,p,helper,owned)
            assert len(owned)==3
            reused=[]
            for k,p in window.SERVICES.items(): window.ensure(k,p,helper,reused)
            assert not reused
            class Foreign(BaseHTTPRequestHandler):
                def do_GET(self): self.send_response(200); self.end_headers(); self.wfile.write(b"other app")
                def log_message(self,*args):pass
            with ThreadingHTTPServer(("127.0.0.2",0),Foreign) as other:
                threading.Thread(target=other.serve_forever,daemon=True).start()
                try:
                    try:window.ensure("studio",other.server_port,helper,reused)
                    except RuntimeError as e: assert "conflicting" in str(e)
                    else:raise AssertionError("Wrong service accepted")
                    assert not reused
                finally:other.shutdown()
            health={"service":"lummix-media","protocol":1,"capabilities":["spotify-player-volume-v1"]}
            class OldMedia(BaseHTTPRequestHandler):
                def log_message(self,*args):pass
                def do_GET(self):
                    self.send_response(200)
                    self.send_header("Access-Control-Allow-Origin",window.ORIGIN)
                    self.end_headers();self.wfile.write(json.dumps(health).encode())
            with ThreadingHTTPServer(("127.0.0.2",0),OldMedia) as legacy:
                threading.Thread(target=legacy.serve_forever,daemon=True).start()
                try:
                    for version in (1,2,3):
                        health["protocol"]=version
                        try:window.ensure("spotify",legacy.server_port,helper,reused)
                        except RuntimeError as e:assert "reopen" in str(e)
                        else:raise AssertionError("Incompatible media helper was reused")
                    health.update(protocol=2,capabilities=sorted(window.MEDIA_CAPABILITIES))
                    window.ensure("spotify",legacy.server_port,helper,reused)
                    health["capabilities"].append("future-feature")
                    window.ensure("spotify",legacy.server_port,helper,reused)
                    assert not reused
                finally:legacy.shutdown()
        finally:
            for child in owned: child.stdin.close()
            for child in owned: assert child.wait(timeout=10)==0
        assert not any(window.listening(p) for p in window.SERVICES.values())
        result={"coldServices":3,"reused":3,"wrongServiceRejected":True,"ownedExitedCleanly":True,
                "staleMediaRejected":True,"optionalPlayerVolumeNotRequired":True,"additionalCompatibleCapabilityAccepted":True,
                "transport":"127.0.0.2; canonical Host and original request code",
                "helperSha256":hashlib.sha256(helper.read_bytes()).hexdigest()}
        print(json.dumps(result));(EVIDENCE/"cold-launch.json").write_text(json.dumps(result,indent=2),encoding="utf-8")

if __name__=="__main__":run()
