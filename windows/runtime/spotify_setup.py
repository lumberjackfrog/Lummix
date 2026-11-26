"""Coordinate the Studio setup click and relay the Rust Spotify setup status."""
from pathlib import Path
import threading

import diagnostics
from spotify_locations import _saved
from spotify_native import provider


class SpotifySetup:
    def __init__(self, root, changed):
        self.root = Path(root)
        self.path = self.root / "Spotify setup" / "SpotifySetup.exe"
        self.changed = changed
        self.lock = threading.Lock()
        self.cancelled = threading.Event()
        self.busy = False
        self.phase = "idle"
        self.cleanup()

    def cleanup(self):
        try:
            native = provider()
            if native is not None:
                native.setup(0, self.root)
        except (OSError, RuntimeError):
            pass  # A launched bootstrapper can still own its cached executable.

    def status(self):
        with self.lock:
            return {"type": "lummix-spotify-setup", "phase": self.phase}

    def begin(self):
        with self.lock:
            if self.busy or self.cancelled.is_set():
                return
            self.busy = True
            self.phase = "checking"
        self.changed()
        threading.Thread(target=self._work, name="Lummix Spotify setup", daemon=True).start()

    def _work(self):
        phase = "failed"
        try:
            phase = self.prepare()
        except (OSError, ValueError, RuntimeError, AttributeError):
            if not self.cancelled.is_set():
                diagnostics.record("spotify_setup_failed")
        finally:
            with self.lock:
                self.busy = False
                self.phase = phase
            self.changed()

    def prepare(self):
        native = provider()
        if native is None:
            raise OSError("The Rust media library is required for Spotify setup")
        return native.setup(1, self.root, _saved().get("desktop"), self.progress)

    def progress(self, stage):
        if stage:
            with self.lock:
                self.phase = {1: "downloading", 2: "installing"}[stage]
            self.changed()
        return not self.cancelled.is_set()
