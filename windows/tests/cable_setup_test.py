"""Simulates Windows install outcomes. It cannot launch setup or open an audio device."""
from contextlib import contextmanager
import threading
import cable_setup as s


def exercise(initial, after, exit_code):
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
    ctrl=s.Controller(lambda:s.shape(current[0]),prepare,install,lambda n:None)
    first=ctrl.begin()
    if initial=="missing":
        assert first["state"]=="preparing" and ctrl.begin()["state"]=="preparing"
        allowed.set();ctrl.thread.join(3)
        assert calls==["download/verify","consent/install"]
    else:
        assert calls==[]
    return ctrl.status()["state"]


for args,expected in [
    (("ready","ready",0),"ready"),(("partial","partial",0),"partial"),(("unavailable","unavailable",0),"unavailable"),
    (("missing","ready",0),"ready"),(("missing","missing",1223),"cancelled"),
    (("missing","missing",17),"failed"),(("missing","partial",3010),"restart-needed"),
    (("missing","missing",0),"restart-needed"),
]:
    assert exercise(*args)==expected,(args,expected)

@contextmanager
def reject():
    raise ValueError("signature failed")
    yield None

c=s.Controller(lambda:s.shape("missing"),reject,lambda f:(_ for _ in ()).throw(AssertionError()),lambda n:None)
c.begin();c.thread.join(3);assert c.status()["state"]=="failed"
try:
    with s.package(lambda:b"wrong-content"):raise AssertionError("A changed package was accepted")
except ValueError as e:
    assert "reviewed package" in str(e)
print("Existing, partial, missing, duplicate, consent denied, signature failure, installer failure, success and reboot cases passed with no installer execution.")
