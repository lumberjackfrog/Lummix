"""Keep a local, atomic recovery copy of each explicitly saved user preference."""
from contextlib import contextmanager
import json
import logging
import msvcrt
import os
from pathlib import Path
import tempfile


def _atomic(path, data):
    fd, name = tempfile.mkstemp(dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(data, stream, ensure_ascii=True, allow_nan=False, separators=(",", ":"))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(name, path)
    finally:
        Path(name).unlink(missing_ok=True)


@contextmanager
def _locked(folder):
    folder.mkdir(parents=True, exist_ok=True)
    with (folder / ".settings-lock").open("a+b") as lock:
        lock.seek(0)
        msvcrt.locking(lock.fileno(), msvcrt.LK_LOCK, 1)
        try:
            yield
        finally:
            lock.seek(0)
            msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)


def _read(path):
    def unique(pairs):
        fields = dict(pairs)
        if len(fields) != len(pairs):
            raise ValueError("Duplicate settings field")
        return fields
    data = json.loads(path.read_text(encoding="utf-8-sig"), object_pairs_hook=unique)
    if not isinstance(data, dict):
        raise ValueError("Expected a settings object")
    return data


def _sections(path):
    try:
        data = _read(path)
        if data.get("version") == 1 and isinstance(data.get("settings"), dict):
            return data["settings"]
    except (FileNotFoundError, ValueError):
        pass
    return {}


def _mirror(path, value):
    backup = path.with_name("settings-backup.json")
    try:
        sections = _sections(backup)
        if sections.get(path.name) != value:
            sections[path.name] = value
            _atomic(backup, {"version": 1, "settings": sections})
    except OSError as error:
        logging.getLogger(__name__).warning("Could not update recovery copy for %s: %s", path.name, error)


def save(path, data):
    with _locked(path.parent):
        _atomic(path, data)
        _mirror(path, data)


def load(path, validate):
    """Validate before mirroring, and repair missing or invalid originals from the local backup."""
    with _locked(path.parent):
        try:
            data = _read(path)
            validate(data)
        except (FileNotFoundError, ValueError, TypeError, KeyError):
            sections = _sections(path.with_name("settings-backup.json"))
            if path.name not in sections:
                raise
            data = sections[path.name]
            if not isinstance(data, dict):
                raise ValueError("Invalid backup section")
            validate(data)
            _atomic(path, data)
        else:
            _mirror(path, data)
        return data
