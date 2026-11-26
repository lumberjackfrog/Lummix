"""Pass settings validation to the native profile and recovery transaction."""
import ctypes
from functools import cache
import json
import logging
import sys

if getattr(sys, "frozen", False):
    from native_host import library, wide_path
else:
    from runtime.native_host import library, wide_path


_SAVE, _LOAD, _UPDATE = range(3)
_INVALID_JSON = 0x10000
_SCHEMA_ERRORS = (0x10001, 0x10002)
_VALIDATE = ctypes.CFUNCTYPE(ctypes.c_uint32, ctypes.c_void_p, ctypes.c_size_t)


@cache
def _transaction():
    function = library().lummix_profile_transaction
    function.argtypes = [ctypes.c_wchar_p, ctypes.c_size_t, ctypes.c_uint32,
                         ctypes.c_char_p, ctypes.c_size_t, _VALIDATE, ctypes.POINTER(ctypes.c_uint32)]
    function.restype = ctypes.c_uint32
    return function


def _run(path, operation, validate=None, data=None):
    encoded = b"" if data is None else json.dumps(
        data, ensure_ascii=True, allow_nan=False, separators=(",", ":"),
    ).encode("ascii")
    accepted, error = None, None

    @_VALIDATE
    def check(pointer, length):
        nonlocal accepted, error
        try:
            candidate = json.loads(ctypes.string_at(pointer, length))
            validate(candidate)
        except (ValueError, TypeError, KeyError) as invalid:
            error = invalid
            return 1
        except BaseException as failure:
            error = failure
            return 2
        accepted = candidate
        return 0

    warnings = ctypes.c_uint32()
    callback = check if validate is not None else _VALIDATE()
    status = _transaction()(*wide_path(path), operation, encoded, len(encoded), callback, ctypes.byref(warnings))
    if warnings.value & 1:
        logging.getLogger(__name__).warning("Could not update recovery copy for %s", path.name)
    if warnings.value & 2:
        logging.getLogger(__name__).warning("Could not restore %s", path.name)
    if status == _INVALID_JSON:
        raise ValueError("Invalid settings JSON")
    if status in _SCHEMA_ERRORS and error is not None:
        raise error
    if status:
        raise ctypes.WinError(status)
    return accepted


def save(path, data):
    _run(path, _SAVE, data=data)


def load(path, validate):
    """Validate before mirroring and recover invalid originals from their backup."""
    return _run(path, _LOAD, validate)


def update(path, changes, validate):
    """Merge changes with the latest valid settings under the native lock."""
    _run(path, _UPDATE, validate, changes)
