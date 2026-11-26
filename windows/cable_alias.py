"""Qualify VB-CABLE before calling the isolated Windows naming worker."""
import ctypes
from functools import cache
import json
from pathlib import Path
import sys
import time

from native_host import library

ALIAS = "Lummix"
ERRORS = ("ok", "name_unavailable", "name_collision", "name_not_applied", "name_timed_out", "name_busy")
_OUTPUT = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_size_t)


class AliasError(Exception):
    pass


@cache
def _api():
    host = library()
    host.lummix_cable_endpoint.argtypes = [ctypes.c_uint32, ctypes.c_char_p, ctypes.c_size_t, _OUTPUT]
    host.lummix_cable_endpoint.restype = ctypes.c_uint32
    host.lummix_cable_private_desktop.argtypes = []
    host.lummix_cable_private_desktop.restype = ctypes.c_int32
    return host


def _call(operation, data):
    payload = json.dumps(data, separators=(",", ":")).encode("ascii")
    captured = None

    @_OUTPUT
    def output(pointer, size):
        nonlocal captured
        captured = ctypes.string_at(pointer, size)

    code = _api().lummix_cable_endpoint(operation, payload, len(payload), output)
    if code or captured is None:
        raise AliasError(ERRORS[code] if 0 < code < len(ERRORS) else "name_unavailable")
    return json.loads(captured)


def endpoint(cable_ids):
    return _call(0, cable_ids)


def signed_names(cable_ids):
    try:
        return _call(1, cable_ids)
    except AliasError:
        return None


def labels(cable_ids):
    try:
        item = endpoint(cable_ids)
        quality = "unavailable" if item["collision"] else "ready" if item["short"] == ALIAS else "original"
        return dict(captureName=item["name"], captureAlias=quality)
    except (AliasError, ImportError, OSError):
        return dict(captureName=None, captureAlias="unavailable")


def apply_in_private_desktop(ident):
    from mic_routes import signed_cables
    if not _api().lummix_cable_private_desktop():
        raise AliasError("name_unavailable")
    _call(2, dict(signed_cables(), workerId=ident))


def worker(ident):
    try:
        apply_in_private_desktop(ident)
        return 0
    except AliasError as error:
        return ERRORS.index(str(error))
    except Exception:
        return 1


def ensure(cable_ids=None):
    """Return Windows' actual post-action friendly name."""
    from endpoint_private_desktop import private_run
    from mic_routes import signed_cables

    cable_ids = cable_ids if cable_ids is not None else signed_cables()
    before = None
    try:
        before = endpoint(cable_ids)
        if before["collision"]:
            raise AliasError("name_collision")
        if before["short"] == ALIAS:
            return dict(captureName=before["name"], captureAlias="ready", aliasError=None)
        args = ["name-cable-worker", before["id"]]
        if not getattr(sys, "frozen", False):
            args[:0] = ["-B", str(Path(__file__).resolve())]
        code = private_run(sys.executable, args, timeout=22)
        error = ERRORS[code] if 0 < code < len(ERRORS) else "name_not_applied"
        until = time.monotonic() + (1 if not code else 0)
        while True:
            after = endpoint(cable_ids)
            if after["id"] == before["id"] and after["short"] == ALIAS:
                return dict(captureName=after["name"], captureAlias="ready", aliasError=None)
            if time.monotonic() >= until:
                raise AliasError(error)
            time.sleep(.1)
    except Exception as error:
        code = str(error) if isinstance(error, AliasError) else "name_unavailable"
        return dict(captureName=before["name"] if before else None,
                    captureAlias="original" if before and before["short"] != ALIAS else "unavailable",
                    aliasError=code)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "name-cable-worker":
        sys.exit(worker(sys.argv[2]))
    if sys.argv[1:] == ["name-cable"]:
        result = ensure()
        print(json.dumps(result))
        sys.exit(0 if result["captureAlias"] == "ready" else 1)
    sys.exit(2)
