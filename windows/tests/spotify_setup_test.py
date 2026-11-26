"""Verify isolated Rust setup operations and Studio's one-click controller."""
from pathlib import Path
from tempfile import TemporaryDirectory
from threading import Event
import unittest
from unittest.mock import Mock, patch

import spotify_setup as setup


class SetupTests(unittest.TestCase):
    def setUp(self):
        root = TemporaryDirectory()
        self.addCleanup(root.cleanup)
        self.controller = setup.SpotifySetup(root.name, Mock())

    def test_only_one_click_runs_native_setup_and_all_phases_relay(self):
        entered, resume, done = Event(), Event(), Event()
        states = []

        def native_work(operation, root, preferred, progress):
            self.assertEqual(operation, 1)
            self.assertEqual(root, self.controller.root)
            self.assertEqual(preferred, r"C:\Verified\Spotify.exe")
            self.assertTrue(progress(1))
            entered.set()
            self.assertTrue(resume.wait(5))
            self.assertTrue(progress(2))
            return "installed"

        def changed():
            states.append(self.controller.status()["phase"])
            if states[-1] == "installed":
                done.set()

        self.controller.changed = changed
        native = Mock()
        native.setup.side_effect = native_work
        with patch.object(setup, "provider", return_value=native), patch.object(
                setup, "_saved", return_value={"desktop": r"C:\Verified\Spotify.exe"}
        ):
            self.controller.begin()
            self.assertTrue(entered.wait(5))
            for _ in range(100):
                self.controller.begin()
            resume.set()
            self.assertTrue(done.wait(5))
        native.setup.assert_called_once()
        self.assertEqual(states, ["checking", "downloading", "installing", "installed"])

    def test_controller_passes_cancellation_back_to_native_worker(self):
        entered, release, done = Event(), Event(), Event()

        def native_work(operation, root, preferred, progress):
            self.assertTrue(progress(1))
            entered.set()
            self.assertTrue(release.wait(5))
            self.assertFalse(progress(0))
            return "idle"

        self.controller.changed = lambda: done.set() if self.controller.status()["phase"] == "idle" else None
        native = Mock()
        native.setup.side_effect = native_work
        with patch.object(setup, "provider", return_value=native):
            self.controller.begin()
            self.assertTrue(entered.wait(5))
            self.controller.cancelled.set()
            release.set()
            self.assertTrue(done.wait(5))
            self.controller.begin()
        native.setup.assert_called_once()

    def test_real_native_discovery_and_unsigned_rejection_without_launch(self):
        native = setup.provider()
        if native is None:
            self.skipTest("Build the Rust media DLL before running Windows integration fixtures")
        discovered = native.worker.submit(native.setup, 2, self.controller.root).result()
        self.assertIsInstance(discovered["running"], bool)
        if discovered["installation"] is not None:
            self.assertIn(discovered["installation"]["edition"], {"desktop", "store"})
        path = self.controller.path
        path.parent.mkdir()
        path.write_bytes(b"MZ unsigned fixture")
        with self.assertRaisesRegex(OSError, "could not verify Spotify AB"):
            native.setup(3, self.controller.root)
        part = path.with_suffix(".part")
        part.write_bytes(b"fixture")
        self.controller.cleanup()
        self.assertFalse(path.exists() or part.exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
