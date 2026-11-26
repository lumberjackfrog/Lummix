"""Smoke-test the actual Rust media DLL against Windows and the HTTP entry point."""
import importlib
import ctypes
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock, patch

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
        with self.assertRaises(OSError):
            native.call(21, 0, 3)  # Reject a stale target before any transport dispatch.
        self.assertEqual(native.call(9, 0, 0, 0), {"reason": "selection_changed"})
        self.assertEqual(native.volume([], None, None), {"reason": "no_player_slider", "http": 409})
        self.assertIsNone(native.direct_to_cable(0, {"invalid-fixture-id"}))
        self.assertIsNone(native.peak([]))
        self.assertTrue(all(handle and pid for handle, pid in native.call(13)))
        self.assertEqual(native.call(14, 0, 0), "")
        source = native.dll.lummix_audio_source
        source.argtypes = (ctypes.c_char_p, ctypes.POINTER(ctypes.c_uint32),
                           ctypes.POINTER(ctypes.c_double), ctypes.POINTER(ctypes.c_uint32))
        source.restype = ctypes.c_int
        pid, created, edition = ctypes.c_uint32(99), ctypes.c_double(99), ctypes.c_uint32(99)
        result = native.worker.submit(source, b'["lummix-impossible-endpoint"]',
                                      ctypes.byref(pid), ctypes.byref(created), ctypes.byref(edition)).result()
        self.assertIn(result, {0, 2, 4})
        self.assertEqual((pid.value, created.value, edition.value), (0, 0.0, 0))
        origin = "http://127.0.0.1:41835"
        store = native.capability(0)
        try:
            token = native.capability(1, store, origin=origin)["token"]
            self.assertRegex(token, r"^[A-Za-z0-9_-]{43}$")
            self.assertTrue(native.capability(2, store, token=token, origin=origin))
            self.assertFalse(native.capability(2, store, token=token, origin="http://localhost:41835"))
            native.capability(3, store, token=token)
            self.assertFalse(native.capability(2, store, token=token, origin=origin))
        finally:
            native.capability(4, store)
        service = importlib.import_module("spotify-helper")
        headers = {"Host": "media-test", "Origin": origin}
        profile = TemporaryDirectory()
        self.addCleanup(profile.cleanup)
        mix = MixOutput(path=Path(profile.name) / "gains.json", mixer=Mock())
        self.enterContext(patch.object(service, "_saved", return_value={}))
        self.enterContext(patch.object(service, "_remember"))
        app = service.create_app(expected_host=headers["Host"], mix_output=mix)
        async with TestClient(TestServer(app)) as client:
            async with client.get("/lummix-health", headers=headers) as reply:
                self.assertEqual(reply.status, 200)
                self.assertIn("spotify-smtc-rust-v1", (await reply.json())["capabilities"])
            async with client.get("/spotify/status", headers=headers) as reply:
                self.assertEqual(reply.status, 200)
                state = await reply.json()
                self.assertIn(state["state"], {"no_session", "ready", "detected", "unavailable", "ambiguous"})
                self.assertNotIn("inventoryPaths", state)
                self.assertNotIn("inventoryRevision", state)
                self.assertNotIn("session", state)
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
