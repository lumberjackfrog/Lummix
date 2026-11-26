"""Bindings for the Rust release, download, and installer workflow."""
import ctypes
from dataclasses import dataclass
from functools import cache
import json
from pathlib import Path
import stat
from urllib.error import URLError

from native_host import library, wide_path


ASSET = "Lummix.Setup.exe"
_LATEST, _PUBLISHED, _DOWNLOAD, _INSTALL, _CLEANUP, _VERIFY = range(6)
_OUTPUT = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_size_t)
_GATE = ctypes.CFUNCTYPE(ctypes.c_int32)


class InvalidUpdate(ValueError):
    pass


@dataclass(frozen=True)
class Release:
    version: str
    size: int
    digest: str
    signed_identity: bytes


@cache
def _native():
    call = library().lummix_update
    call.argtypes = [ctypes.c_uint32, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_wchar_p,
                     ctypes.c_size_t, _OUTPUT, _GATE]
    call.restype = ctypes.c_uint32
    return call


def _request(action, payload=b"", directory=None, allowed=None):
    captured, callback_error = None, None

    @_OUTPUT
    def output(pointer, size):
        nonlocal captured, callback_error
        try:
            captured = ctypes.string_at(pointer, size)
        except Exception as error:
            callback_error = error

    @_GATE
    def gate():
        nonlocal callback_error
        try:
            return bool(allowed())
        except Exception as error:
            callback_error = error
            return False

    path, length = wide_path(directory) if directory is not None else (None, 0)
    status = _native()(action, payload, len(payload), path, length, output,
                       gate if allowed is not None else _GATE())
    if callback_error is not None:
        raise callback_error
    if status == 1:
        raise InvalidUpdate("The official Lummix release could not be verified.")
    if status == 2:
        raise URLError("Could not reach the official Lummix release.")
    if status == 3:
        raise OSError("Windows could not access or open the staged Lummix installer.")
    if status == 4:
        raise InterruptedError("The update was cancelled or the mixer is busy.")
    if status not in (0, 5):
        raise OSError("The native Lummix updater returned an unknown result.")
    return captured


def _release(identity):
    if identity is None:
        return None
    document = json.loads(identity)
    return Release(document["tag"][1:], document["size"], document["sha256"], identity)


def latest(current):
    return _release(_request(_LATEST, current.encode("ascii")))


def published(tag):
    return _release(_request(_PUBLISHED, tag.encode("ascii")))


def signed_release(document):
    """Verify an archived publisher manifest without network or installation."""
    encoded = json.dumps(document, separators=(",", ":")).encode("ascii")
    return _release(_request(_VERIFY, encoded))


def download(release, directory, cancelled):
    _request(_DOWNLOAD, release.signed_identity, directory, lambda: not cancelled.is_set())
    return Path(directory) / ASSET


def install(release, directory, cancelled):
    try:
        _request(_INSTALL, release.signed_identity, directory, lambda: not cancelled.is_set())
    except InterruptedError:
        return False
    return True


def cleanup(directory):
    _request(_CLEANUP, directory=directory)


def plain(path):
    """Used by Spotify setup and the best-effort release-check timestamp."""
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return
    if getattr(metadata, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT:
        raise InvalidUpdate("unexpected cache link")
