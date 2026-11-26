"""Smoke-test the actual Rust media DLL against Windows and the HTTP entry point."""
import importlib
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock

from aiohttp.test_utils import TestClient, TestServer

from mix_gains_http import MixOutput
from spotify_native import provider


class NativeMediaTests(unittest.IsolatedAsyncioTestCase):
    async def test_windows_media_provider(self):
        native = provider()
        self.assertIsNotNone(native, "Build windows/media_native first")
        # These calls are read-only and work on an empty, clean runner.
        self.assertTrue(all(path is None or isinstance(path, str) for path in native.call(5)))
        self.assertTrue(all(field in ("", "InstallLocation", "DisplayIcon") and isinstance(value, str)
                            for field, value in native.call(8)))
        with self.assertRaises(OSError):
            native.call(1, 0)
        self.assertEqual(native.call(9, 0, 0, 0), {"reason": "selection_changed"})
        self.assertEqual(native.volume([], None, None), {"reason": "no_player_slider", "http": 409})
        self.assertIsNone(native.direct_to_cable(0, {"invalid-fixture-id"}))
        self.assertIsNone(native.peak([]))
        self.assertTrue(all(handle and pid for handle, pid in native.call(13)))
        self.assertEqual(native.call(14, 0, 0), "")
        service = importlib.import_module("spotify-helper")
        headers = {"Host": "media-test", "Origin": "http://127.0.0.1:41835"}
        profile = TemporaryDirectory()
        self.addCleanup(profile.cleanup)
        mix = MixOutput(path=Path(profile.name) / "gains.json", mixer=Mock())
        app = service.create_app(expected_host=headers["Host"], inventory=lambda: ([], False), mix_output=mix)
        async with TestClient(TestServer(app)) as client:
            async with client.get("/lummix-health", headers=headers) as reply:
                self.assertEqual(reply.status, 200)
                self.assertIn("spotify-smtc-rust-v1", (await reply.json())["capabilities"])
            async with client.get("/spotify/status", headers=headers) as reply:
                self.assertEqual(reply.status, 200)
                state = await reply.json()
                self.assertIn(state["state"], {"no_session", "ready", "detected", "unavailable", "ambiguous"})
                if state["state"] == "ready":
                    self.assertIn(state["edition"], {"desktop", "store"})
                    self.assertEqual(len(state["token"]), 43)
                    async with client.get(
                        "/spotify/peak", headers={**headers, "X-Lum-Spotify-Token": state["token"]}
                    ) as peak:
                        self.assertEqual(peak.status, 200)
                        self.assertIn((await peak.json())["state"], {"ready", "paused", "no_local_signal"})
            async with client.get("/spotify/changes", headers=headers) as reply:
                self.assertEqual(reply.status, 200)
                state = await reply.json()
                self.assertIsInstance(state["events"], bool)
                self.assertTrue(state["revision"])


if __name__ == "__main__":
    unittest.main()
