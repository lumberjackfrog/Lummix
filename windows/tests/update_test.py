"""Release authenticity, streaming limits, idle install and local log retention."""
import base64
import gc
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch

import psutil
import client_update
import diagnostics
import update_feed as feed

# A real signature over the previously published 1.0.7 Windows installer.
HISTORICAL = {"format":1,"tag":"v1.0.7","size":42737340,
              "sha256":"d25f4c5000e99ab5a440f3db8bc957adb24b3a7b8aee68991fd2e6ab3bf0c907",
              "signature":"Owlkc2kTdwAmmij6niAa4O59y8smF109wS+WzQ1Vo34Mzt5Lt/w0GvufpnDtwjrywhIgFHnAV2ak9Zz37e7kdg=="}


def validate(doc):
    return feed.signed_release(doc)


class ReleaseTests(unittest.TestCase):
    def test_signed_identity_and_tampering(self):
        self.assertEqual(validate(HISTORICAL).version,"1.0.7")
        for change in ({"tag":"v9.0.7"},{"sha256":"0"*64},{"size":123},{"format":True},{"signature":"!"},{"signature":base64.b64encode(bytes(64)).decode()}):
            with self.subTest(change=list(change)), self.assertRaises(feed.InvalidUpdate):
                validate(HISTORICAL | change)

    def test_coalesced_checks_and_cleanup(self):
        entered,finish=threading.Event(),threading.Event()
        def check(current):
            entered.set()
            self.assertTrue(finish.wait(5))
            return None
        with tempfile.TemporaryDirectory() as path, patch.object(feed,"latest",side_effect=check) as query:
            changed=threading.Event()
            updates=client_update.Updates(path,changed.set)
            updates.command("check")
            self.assertTrue(entered.wait(5))
            for _ in range(1000): updates.command("check")
            changed.clear()
            finish.set()
            self.assertTrue(changed.wait(5))
            self.assertEqual(updates.status()["phase"],"current")
            self.assertEqual(query.call_count,1)
            (updates.directory/"download.part").write_text("partial")
            (updates.directory/feed.ASSET).write_text("old")
            other=client_update.Updates(path,changed.set)
            stamp=(updates.directory/"last-check").stat().st_mtime
            with patch.object(client_update.time,"time",return_value=stamp-.005):
                other.command("auto")
            self.assertEqual(other.status()["phase"],"current")
            self.assertEqual(query.call_count,1)
            self.assertEqual([p.name for p in updates.directory.iterdir()],["last-check"])
            finish.clear()
            entered.clear()
            with patch.object(client_update.time,"time",return_value=stamp-3600):
                other.command("auto")
            self.assertTrue(entered.wait(5))
            changed.clear()
            finish.set()
            self.assertTrue(changed.wait(5))
            self.assertEqual(query.call_count,2)

    def test_log_disk_cap_privacy_and_native_handle_lifetime(self):
        summary=diagnostics.audio_summary({"routeActive":True,"cableReady":False,"spotifyCapture":"private title","token":"secret","inputs":["My Mic"]})
        self.assertEqual(summary,"audio_snapshot mix=yes cable=no spotify=unknown")
        with tempfile.TemporaryDirectory() as path, patch.dict(os.environ,LOCALAPPDATA=path):
            validate(HISTORICAL)
            process=psutil.Process()
            handles=process.num_handles()
            for _ in range(300): validate(HISTORICAL)
            gc.collect()
            self.assertLessEqual(process.num_handles(),handles+4)
            for _ in range(3200): diagnostics.record("opened")
            diagnostics.record("C:/private/user-name/music.mp3")
            logs=list(diagnostics.folder().glob("*.log"))
            self.assertEqual(len(logs),2)
            self.assertLessEqual(sum(p.stat().st_size for p in logs),2*diagnostics.LIMIT)
            self.assertTrue(all("user-name" not in p.read_text() for p in logs))
            diagnostics.clear()
            self.assertFalse(list(diagnostics.folder().iterdir()))


if __name__=="__main__":
    unittest.main(verbosity=2)
