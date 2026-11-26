"""The setup click can only install Spotify's signed website player when one is missing."""
from contextlib import ExitStack
from io import BytesIO
from pathlib import Path
from tempfile import TemporaryDirectory
from threading import Event
import unittest
from unittest.mock import Mock, patch

import spotify_setup as s


class Response(BytesIO):
    status = 200
    headers = {}

    def geturl(self):
        return s.URL


class SetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.controller = s.SpotifySetup(self.temp.name, Mock())
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)

    def mock(self, name, **kwargs):
        return self.stack.enter_context(patch.object(s, name, **kwargs))

    def test_reuse_both_editions_and_skip_running_player(self):
        self.mock('ctypes').windll.shell32.IsUserAnAdmin.return_value = False
        detect = self.mock('installed')
        active = self.mock('running', return_value=True)
        download = self.mock('download')
        self.assertEqual(self.controller.prepare(), 'ready')
        detect.assert_not_called()
        active.return_value = False
        desktop = Path(self.temp.name) / 'Spotify.exe'
        detect.return_value = desktop
        signed = self.mock('open_verified')
        self.assertEqual(self.controller.prepare(), 'opened')
        signed.assert_called_once_with(desktop, self.controller.cancelled)
        detect.return_value = 'spotify:'
        with patch.object(s.os, 'startfile') as shell:
            self.assertEqual(self.controller.prepare(), 'opened')
            shell.assert_called_once_with('spotify:', 'open')
        download.assert_not_called()

    def test_install_only_once_and_verify_detection_after_launch(self):
        self.mock('ctypes').windll.shell32.IsUserAnAdmin.return_value = False
        self.mock('running', return_value=False)
        self.mock('installed', side_effect=[None, None, Path('Spotify.exe')])
        entered, resume, done = Event(), Event(), Event()
        def stage(*args):
            entered.set()
            self.assertTrue(resume.wait(5))
        download = self.mock('download', side_effect=stage)
        launch = self.mock('open_verified')
        self.controller.changed = lambda: done.set() if self.controller.status()['phase']=='installed' else None
        self.controller.begin()
        self.assertTrue(entered.wait(3))
        for _ in range(100): self.controller.begin()
        resume.set()
        self.assertTrue(done.wait(6))
        self.assertEqual(download.call_count, 1)
        launch.assert_called_once_with(self.controller.path, self.controller.cancelled)

    def test_no_elevated_install_and_no_launch_if_identity_is_unknown(self):
        self.mock('running', return_value=False)
        detect = self.mock('installed', return_value=None)
        download = self.mock('download')
        self.mock('ctypes').windll.shell32.IsUserAnAdmin.return_value = True
        self.assertEqual(self.controller.prepare(), 'elevated')
        detect.side_effect = OSError('cannot inspect the current account')
        with self.assertRaises(OSError): self.controller.prepare()
        download.assert_not_called()

    def test_download_limits_origin_cancellation_and_publisher(self):
        path, cancel = self.controller.path, self.controller.cancelled
        def run(response):
            s.download(path, cancel, lambda *a, **k: response)
        bad = Response(b'MZ')
        bad.geturl = lambda: 'https://example.org/app.exe'
        with self.assertRaises(ValueError): run(bad)
        for length in ['0','3',str(s.MAX_BYTES+1)]:
            bad = Response(b'MZ')
            bad.headers = {'Content-Length':length}
            with self.assertRaises(ValueError): run(bad)
        cancel.set()
        with self.assertRaises(InterruptedError): run(Response(b'MZ'))
        cancel.clear()
        self.assertFalse(path.exists() or path.with_suffix('.part').exists())
        run(Response(b'MZ'))
        self.assertIn('ZoneId=3',Path(str(path)+':Zone.Identifier').read_text())
        with patch.object(s.os,'startfile') as launch:
            with self.assertRaises(ValueError): s.open_verified(path, cancel)
            launch.assert_not_called()
        self.controller.cleanup()
        self.assertFalse(path.exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
