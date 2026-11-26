"""Lifetime and status of the Rust process-specific Spotify source."""
import ctypes
import json
import os
import sys
import threading
from weakref import finalize

from cable_mixer import Controls
from native_audio import Engine, _release_owned


def supported():
    return os.name == "nt" and sys.getwindowsversion().build >= 20348


class SpotifyProcessFeed:
    states = ("connecting","waiting_spotify","waiting_audio","receiving","direct_to_cable","unavailable","ambiguous")

    def __init__(self, signed_render_ids, *, engine=None, probe=None):
        self.engine = engine if engine is not None else Engine(Controls())
        self.lock = threading.Lock()
        self.handle = None
        self.inactive = "unsupported" if not supported() else "unavailable"
        if not supported():
            return
        if probe is None:
            from spotify_native import provider
            try:
                media = provider()
                if media is None:
                    return
                probe = media.dll.lummix_audio_source
            except (OSError, AttributeError, RuntimeError):
                return
        self.probe = probe
        self.handle = self.engine.dll.lummix_watch_start(self.engine.handle,json.dumps(sorted(signed_render_ids)).encode(),
                                                       ctypes.cast(probe, ctypes.c_void_p))
        if self.handle:
            self._release = finalize(self, _release_owned, self.engine.dll.lummix_watch_close,
                                     self.handle, self.engine, probe)

    def _status(self):
        with self.lock:
            return self.engine.dll.lummix_watch_status(self.handle) if self.handle else None

    @property
    def state(self):
        value = self._status()
        return self.states[value & 255] if value is not None else self.inactive

    @property
    def edition(self):
        value = self._status()
        return {1:"desktop",2:"store"}.get(value >> 8) if value is not None else None

    def next(self):
        return self.engine.read() if self.state in ("receiving","waiting_audio","connecting") else None

    def close(self):
        with self.lock:
            self.inactive = "off"
            if self.handle:
                self._release()
                self.handle = None
