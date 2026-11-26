"""Release authenticity, streaming limits, idle install and local log retention."""
import base64
import gc
import hashlib
import io
import json
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
    return feed.signed_release(doc,doc["tag"],doc["size"],doc["sha256"])


def release_row():
    tag = HISTORICAL["tag"]
    assets = [{"name":name,"browser_download_url":f"{feed.REPO}/download/{tag}/{name}",
               "state":"uploaded","size":size,"digest":"sha256:"+HISTORICAL["sha256"]}
              for name,size in ((feed.ASSET,HISTORICAL["size"]),(feed.MANIFEST,300))]
    return {"draft":False,"prerelease":True,"tag_name":tag,"published_at":"2026-11-26T00:00:00Z",
            "html_url":feed.REPO+"/tag/"+tag,"assets":assets}


class ReleaseTests(unittest.TestCase):
    def test_signed_identity_and_tampering(self):
        self.assertEqual(validate(HISTORICAL).version,"1.0.7")
        for change in ({"tag":"v9.0.7"},{"sha256":"0"*64},{"size":123},{"format":True},{"signature":"!"},{"signature":base64.b64encode(bytes(64)).decode()}):
            with self.subTest(change=list(change)), self.assertRaises(feed.InvalidUpdate):
                validate(HISTORICAL | change)

    def test_feed_and_no_downgrade(self):
        row=release_row()
        def read(url):
            return io.BytesIO(json.dumps([row] if url==feed.FEED else HISTORICAL).encode())
        self.assertEqual(feed.latest("1.0.6",read),validate(HISTORICAL))
        self.assertIsNone(feed.latest("1.0.7",read))
        self.assertIsNone(feed.latest("1.0.8",read))
        row["draft"]=True
        self.assertIsNone(feed.latest("1.0.6",read))
        row["draft"]=False
        row["assets"][0]["browser_download_url"]="https://example.org/setup.exe"
        with self.assertRaises(feed.InvalidUpdate): feed.latest("1.0.6",read)
        row["assets"]=[]
        with self.assertRaises(feed.InvalidUpdate): feed.latest("1.0.6",read)
        for url in ("http://github.com/a","https://github.com.evil.invalid/a","https://user@github.com/a","file:///C:/a","https://github.com:8443/a"):
            self.assertFalse(feed.safe_url(url))

    def test_stream_integrity_cancellation_and_idle_install(self):
        data=b"MZ"+bytes(2*feed.CHUNK)
        release=feed.Release("1.0.9",len(data),hashlib.sha256(data).hexdigest())
        stop=threading.Event()
        with tempfile.TemporaryDirectory() as root:
            root=Path(root)
            for bad in (data[:-1],data+b"X",b"x"+data[1:]):
                with self.assertRaises(feed.InvalidUpdate):
                    feed.download(release,root,stop,lambda url:io.BytesIO(bad))
                self.assertFalse(list(root.iterdir()))
            stop.set()
            with self.assertRaises(InterruptedError):
                feed.download(release,root,stop,lambda url:io.BytesIO(data))
            stop.clear()
            path=feed.download(release,root,stop,lambda url:io.BytesIO(data))
            self.assertIn("ZoneId=3",Path(str(path)+":Zone.Identifier").read_text())
            launches=[]
            def shell(*args): launches.append(args)
            self.assertFalse(client_update.launch_installer(release,path,stop,idle=lambda:False,shell=shell))
            idle=iter((True,False))
            self.assertFalse(client_update.launch_installer(release,path,stop,idle=lambda:next(idle),shell=shell))
            self.assertFalse(launches)
            self.assertTrue(client_update.launch_installer(release,path,stop,idle=lambda:True,shell=shell))
            self.assertEqual(launches,[(str(path),"open",f"/LUMMIXWAITPID={os.getpid()}")])
            path.write_bytes(b"X"+data[1:])
            with self.assertRaises(feed.InvalidUpdate):
                client_update.launch_installer(release,path,stop,idle=lambda:True,shell=shell)
            self.assertEqual(len(launches),1)

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
            other.command("auto")
            self.assertEqual(query.call_count,1)
            self.assertEqual([p.name for p in updates.directory.iterdir()],["last-check"])

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
