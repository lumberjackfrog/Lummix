"""Simulates Windows install outcomes. It cannot launch setup or open an audio device."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
from tempfile import TemporaryDirectory
import threading
from unittest.mock import patch
import cable_setup as s
from mic_routes import signed_cables


def exercise(initial, after, exit_code, marker, boot=None):
    calls=[]; current=[initial]; allowed=threading.Event()
    @contextmanager
    def prepare():
        calls.append("download/verify")
        allowed.wait(3)
        yield None
    def install(folder):
        calls.append("consent/install")
        current[0]=after
        return exit_code
    boot = boot or {"event":"1:100","ticks":100_000_000}
    ctrl=s.Controller(lambda:s.shape(current[0]) | {"boot":boot},prepare,install,marker)
    first=ctrl.begin()
    if initial=="missing":
        assert first["state"]=="preparing" and ctrl.begin()["state"]=="preparing"
        allowed.set();ctrl.thread.join(3)
        assert calls==["download/verify","consent/install"]
    else:
        assert calls==[]
    state = ctrl.status()["state"]
    assert marker.exists() == (state=="restart-needed")
    return state


for args,expected in [
    (("ready","ready",0),"ready"),(("partial","partial",0),"partial"),(("unavailable","unavailable",0),"unavailable"),
    (("missing","ready",0),"restart-needed"),(("missing","missing",1223),"cancelled"),
    (("missing","missing",17),"failed"),(("missing","ready",17),"restart-needed"),
    (("missing","partial",3010),"restart-needed"),
    (("missing","ready",3010),"restart-needed"),(("missing","ready",1641),"restart-needed"),
    (("missing","missing",0),"restart-needed"),
]:
    with TemporaryDirectory() as folder:
        assert exercise(*args, Path(folder)/"vb-cable-restart")==expected,(args,expected)

@contextmanager
def reject():
    raise ValueError("signature failed")
    yield None

with TemporaryDirectory() as folder:
    marker = Path(folder)/"vb-cable-restart"
    c=s.Controller(lambda:s.shape("missing"),reject,lambda f:(_ for _ in ()).throw(AssertionError()),marker)
    c.begin();c.thread.join(3);assert c.status()["state"]=="failed"

with TemporaryDirectory() as folder, patch.dict(os.environ, {"LOCALAPPDATA":folder}):
    marker = s.restart_file()
    assert exercise("missing","ready",0,marker)=="restart-needed"
    boot = [{"event":"1:100","ticks":100_000_000}]
    def probe():
        return s.shape("ready") | {"boot":boot[0]}
    render = "{8078C36F-159B-4CC8-A389-4497502275AB}"
    capture = "{07FB90E9-40E1-4E34-ABCE-AF9065BAD550}"
    endpoint = dict(state=1, hardware="VBAudioVACWDM", description="VB-Audio Virtual Cable", inf="oem42.inf")
    driver = dict(name="VB-Audio Virtual Cable", hardware="VBAudioVACWDM", provider="VB-Audio Software",
                  signed=True, signer="Microsoft Windows Hardware Compatibility Publisher", inf="OEM42.INF")
    def inventory():
        data = dict(boot=boot[0], drivers=[driver],
                    endpoints={"Render":[endpoint | {"key":render}], "Capture":[endpoint | {"key":capture}]})
        return s.check(0,data)
    reopened=s.Controller(probe,reject,marker=marker)
    assert reopened.status()["state"]=="restart-needed"
    assert reopened.begin()["state"]=="restart-needed" and reopened.thread is None
    with patch.object(s,"qualified_pair",side_effect=inventory), patch("cable_alias.labels",return_value={}):
        assert signed_cables()=={"error":"cable_restart_needed"}
        boot[0] = {"event":"2:200","ticks":900_000_000}
        assert signed_cables()=={"Render":["{0.0.0.00000000}."+render],"Capture":["{0.0.1.00000000}."+capture]}
        driver["signer"]="Untrusted Publisher"
        assert signed_cables()=={}
        assert s.inspect_pair()["state"]=="partial"
        assert s.Controller(probe,reject,marker=marker).status()["state"]=="ready"
        assert not marker.exists()

with TemporaryDirectory() as folder:
    marker = Path(folder)/"vb-cable-restart"
    boot = {"event":None,"ticks":100_000_000}
    assert exercise("missing","ready",17,marker,boot)=="restart-needed"
    assert json.loads(marker.read_text())==boot
    for current in (boot, {"event":"reappeared","ticks":106_000_000}, None):
        assert s.Controller(lambda:s.shape("ready") | {"boot":current},reject,marker=marker).status()["state"]=="restart-needed"
        assert marker.exists()
    restarted = {"event":None,"ticks":900_000_000}
    assert s.Controller(lambda:s.shape("ready") | {"boot":restarted},reject,marker=marker).status()["state"]=="ready"
    assert not marker.exists()

try:
    with s.package(lambda:b"wrong-content"):raise AssertionError("A changed package was accepted")
except ValueError as e:
    assert "reviewed package" in str(e)
print("Existing, partial, missing, duplicate, consent denied, signature failure, installer failure, same-boot reopen, and restart cases passed with no installer execution.")
