import concurrent.futures
import json
import os
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock, patch

from mic_routes import MicRoutes, write_selection
from mix_duck_http import MixDucking
from mix_gains_http import MixOutput
import profile_save
import spotify_locations as spotify
from studio_preferences import StudioPreferences


class Recovery(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(TemporaryDirectory()))
        self.gains = self.root / "output-gains.json"
        self.backup = self.root / "settings-backup.json"

    def test_legacy_import_damage_and_restart(self):
        self.gains.write_text('{"revision":7,"gains":{"micGain":160,"musicMuted":true}}')
        mixer = MixOutput(self.gains, Mock())
        self.assertEqual(7, mixer.revision)
        prefs = StudioPreferences(mixer)
        prefs.save({"snap": 5, "closeToTray": False})
        duck = MixDucking(mixer, Mock())
        duck.write({"enabled": True, "depthDb": 14})
        mic = MicRoutes(mixer)
        write_selection(mic.path, "fixture device")
        self.gains.write_text('{"revision":-1,"gains":{}}')
        prefs.path.unlink()
        duck.path.write_bytes(b"{")
        mic.path.write_text("[]")
        restarted = MixOutput(self.gains, Mock())
        self.assertEqual((160, True, 7), (restarted.gains["micGain"], restarted.gains["musicMuted"], restarted.revision))
        self.assertEqual({"snap": 5, "closeToTray": False}, StudioPreferences(restarted).read())
        self.assertEqual(14, MixDucking(restarted, Mock()).settings["depthDb"])
        self.assertEqual("fixture device", MicRoutes(restarted).saved)
        self.assertTrue(all(p.is_file() for p in (self.gains, prefs.path, duck.path, mic.path)))
        self.assertNotIn("token", self.backup.read_text())

    def test_distinct_writers_and_corrupt_backup(self):
        rows = [self.root / name for name in ("studio-preferences.json", "window-placement.json", "spotify-locations.json")]
        with concurrent.futures.ThreadPoolExecutor() as workers:
            list(workers.map(lambda p: profile_save.save(p, {"id": p.name}), rows))
        self.assertEqual(set(json.loads(self.backup.read_bytes())["settings"]), {p.name for p in rows})
        self.backup.write_bytes(b"!")
        for path in rows:
            self.assertEqual(path.name, profile_save.load(path, lambda data: data["id"])["id"])
        self.assertEqual(len(json.loads(self.backup.read_bytes())["settings"]), 3)

    def test_spotify_moves_and_stale_or_invalid_paths_never_count(self):
        env = {"APPDATA": str(self.root / "roaming"), "LOCALAPPDATA": str(self.root / "local")}
        with patch.dict(os.environ, env), patch.object(spotify, "registered_desktop_paths", return_value=[]):
            first = Path(env["APPDATA"]) / "Spotify/Spotify.exe"
            moved = Path(env["LOCALAPPDATA"]) / "Programs/Spotify/Spotify.exe"
            first.parent.mkdir(parents=True)
            first.touch()
            self.assertTrue(spotify.website_installed())
            location = spotify._file()
            self.assertEqual(json.loads(location.read_bytes())["desktop"], str(first))
            location.unlink()
            self.assertTrue(spotify.website_installed())
            self.assertEqual(json.loads(location.read_bytes())["desktop"], str(first))
            first.unlink()
            self.assertFalse(spotify.website_installed())
            moved.parent.mkdir(parents=True)
            moved.touch()
            self.assertTrue(spotify.website_installed())
            self.assertEqual(json.loads(location.read_bytes())["desktop"], str(moved))
            location.write_text("null")
            location.with_name("settings-backup.json").write_text("?")
            self.assertTrue(spotify.website_installed())
            self.assertEqual(json.loads(location.read_bytes())["desktop"], str(moved))

    def test_store_upgrade_and_registered_desktop(self):
        env = {"LOCALAPPDATA": str(self.root / "user"), "APPDATA": str(self.root / "roaming"),
               "ProgramFiles": str(self.root / "programs"), "ProgramFiles(x86)": "", "ProgramW6432": ""}
        with patch.dict(os.environ, env):
            for version in ("1.2.3.0", "1.2.4.0"):
                folder = self.root / "programs/WindowsApps" / f"SpotifyAB.SpotifyMusic_{version}_x64__zpdnekdrzrea0"
                folder.mkdir(parents=True)
                exe = folder / "Spotify.exe"
                exe.touch()
                with patch.object(spotify, "store_packages", return_value=[Mock(installed_location=Mock(path=str(folder)))]):
                    self.assertTrue(spotify.store_installed())
                    self.assertTrue(spotify.known_process_path(exe, os.environ))
                    self.assertEqual(spotify._saved()["store"], str(exe))
            custom = self.root / "custom/Spotify.exe"
            custom.parent.mkdir()
            custom.touch()
            with patch.object(spotify, "registered_desktop_paths", return_value=[custom]):
                self.assertTrue(spotify.website_installed())
                self.assertTrue(spotify.known_process_path(custom, os.environ))
            with patch.object(spotify, "registered_desktop_paths", return_value=[]):
                self.assertFalse(spotify.website_installed())
                self.assertFalse(spotify.known_process_path(custom, os.environ))


if __name__ == "__main__":
    unittest.main()
