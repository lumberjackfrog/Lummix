"""File-level transactions on a synthetic Steam tree. No installed XSOverlay is touched."""
from contextlib import contextmanager
import ctypes
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


sys.path.insert(0,str(Path(__file__).resolve().parents[1]/"windows"))
import xsoverlay_install as addon


@contextmanager
def prevent_replacement(path):
    """Permit inspection while withholding the Windows DELETE sharing right."""
    open_file = ctypes.WinDLL("kernel32", use_last_error=True).CreateFileW
    open_file.argtypes = [ctypes.c_wchar_p, ctypes.c_uint, ctypes.c_uint, ctypes.c_void_p,
                          ctypes.c_uint, ctypes.c_uint, ctypes.c_void_p]
    open_file.restype = ctypes.c_void_p
    handle = open_file(str(path), 0x80000000, 3, None, 3, 0x80, None)
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    close = ctypes.WinDLL("kernel32").CloseHandle
    close.argtypes = [ctypes.c_void_p]
    try:
        yield
    finally:
        close(handle)


class AddonTests(unittest.TestCase):
    def setUp(self):
        temporary=tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root=Path(temporary.name)
        self.apps=root/"Applications"
        self.wrist=self.apps/"_UI"/"Default"/"Wrist.html"
        self.wrist.parent.mkdir(parents=True)
        # Compatibility signatures only; these are not vendor code or assets.
        self.stock=(b'\xef\xbb\xbf<html>\r\n<body id="uiContainer">\r\n'
                    b'<!-- ./_Shared/js/toolbar.js ./_Shared/js/uiComponents.js -->\r\n</body>\r\n</html>')
        self.wrist.write_bytes(self.stock)
        self.assets=root/"assets"
        self.assets.mkdir()
        self.js=self.assets/"xsoverlay-live.js"
        self.js.write_text("/* first fixture */","utf-8")
        (self.assets/"xsoverlay-icon.png").write_bytes(b"\x89PNG\r\n\x1a\ntest bytes")
        self.manager=addon.AddonInstall(lambda:self.apps,self.assets)
        self.directory=self.apps/"Lummix"

    def install(self):
        self.assertEqual(self.manager.change("install")["state"],"ready")

    def test_install_schema_idempotency_and_byte_exact_removal(self):
        self.assertTrue(self.manager.status()["canInstall"])
        self.assertFalse(self.directory.exists())
        self.install()
        self.install()
        manifest=json.loads((self.directory/"app.json").read_bytes())
        self.assertEqual(manifest["url"],"http://127.0.0.1:41839/wrist")
        self.assertEqual(manifest["name"],"Lummix")
        self.assertFalse(manifest["canOpenMultiple"])
        self.assertEqual((self.directory/"icon.png").read_bytes(),(self.assets/"xsoverlay-icon.png").read_bytes())
        self.assertEqual(self.wrist.read_bytes().count(addon.BEGIN),1)
        self.assertEqual(self.manager.change("remove")["state"],"installable")
        self.assertEqual(self.wrist.read_bytes(),self.stock)
        self.assertFalse(self.directory.exists())

    def test_unrelated_edits_survive_update_and_remove(self):
        self.install()
        self.wrist.write_bytes(b"<!-- user's theme -->\n"+self.wrist.read_bytes())
        (self.directory/"notes.txt").write_bytes(b"leave this alone")
        self.js.write_text("/* second fixture */","utf-8")
        self.assertTrue(self.manager.status()["canUpdate"])
        self.install()
        self.assertIn(b"second fixture",self.wrist.read_bytes())
        self.assertNotIn(b"first fixture",self.wrist.read_bytes())
        self.manager.change("remove")
        self.assertEqual((self.directory/"notes.txt").read_bytes(),b"leave this alone")
        self.assertEqual(self.wrist.read_bytes(),b"<!-- user's theme -->\n"+self.stock)

    def test_steam_repair_restores_lost_marker_and_assets(self):
        self.install()
        self.wrist.write_bytes(self.stock)
        (self.directory/"icon.png").unlink()
        self.assertTrue(self.manager.status()["canUpdate"])
        self.install()
        self.assertEqual(self.wrist.read_bytes().count(addon.BEGIN),1)

    def test_only_current_marker_can_recover_if_folder_lost(self):
        self.install()
        for p in self.directory.iterdir():
            p.unlink()
        self.directory.rmdir()
        self.assertTrue(self.manager.status()["canUpdate"])
        self.install()

    def test_preserves_unrecognized_directory(self):
        self.directory.mkdir()
        (self.directory/"app.json").write_bytes(b"another application")
        self.assertEqual(self.manager.change("install")["state"],"conflict")
        self.assertEqual((self.directory/"app.json").read_bytes(),b"another application")
        self.assertEqual(self.wrist.read_bytes(),self.stock)

    def test_preserves_user_changes_in_owned_block_or_file(self):
        self.install()
        installed=self.wrist.read_bytes()
        edit=installed.replace(b"first fixture",b"custom code")
        self.wrist.write_bytes(edit)
        self.assertEqual(self.manager.change("remove")["state"],"conflict")
        self.assertEqual(self.wrist.read_bytes(),edit)
        self.wrist.write_bytes(installed)
        (self.directory/"app.json").write_bytes(b"custom manifest")
        self.assertEqual(self.manager.change("install")["state"],"conflict")
        self.assertEqual((self.directory/"app.json").read_bytes(),b"custom manifest")

    def test_partial_markers_and_incompatible_layout(self):
        self.wrist.write_bytes(self.stock+addon.BEGIN)
        self.assertEqual(self.manager.change("install")["state"],"conflict")
        self.wrist.write_bytes(b"<html><body></body></html>")
        self.assertEqual(self.manager.change("install")["state"],"unsupported")
        self.assertFalse(self.directory.exists())
        self.assertEqual(addon.AddonInstall(lambda:None,self.assets).status()["state"],"missing")

    def test_partial_write_failure_reverts_whole_install(self):
        with prevent_replacement(self.wrist):
            self.assertEqual(self.manager.change("install")["state"],"blocked")
        self.assertEqual(self.wrist.read_bytes(),self.stock)
        self.assertFalse(self.directory.exists())
        self.assertFalse(list(self.wrist.parent.glob(".lummix-*.tmp")))

    def test_partial_uninstall_rolls_back(self):
        self.install()
        before=self.wrist.read_bytes()
        with prevent_replacement(self.directory/"icon.png"):
            self.assertEqual(self.manager.change("remove")["state"],"blocked")
        self.assertEqual(self.wrist.read_bytes(),before)
        self.assertEqual(self.manager.status()["state"],"ready")

    def test_legacy_manifest_and_receipt_stay_byte_exact(self):
        self.install()
        manifest = {"author":"Lummix","name":"Lummix","version":"v1.0",
                    "description":"Lummix microphone and music controls.",
                    "url":"http://127.0.0.1:41839/wrist","resWidth":720,"resHeight":480,
                    "iconColor":[.68,.52,.78],"defaultScale":.2,"canOpenMultiple":False,"canBeCurved":False}
        raw = (json.dumps(manifest, indent=2) + "\n").encode()
        digest = lambda data: hashlib.sha256(data).hexdigest()
        block = (addon.BEGIN + b"\n<script>\n" + self.js.read_bytes()
                 + b"\n</script>\n<!-- Lummix wrist add-on END -->")
        receipt = {"owner":"Lummix","format":1,"block":digest(block),
                   "files":{"app.json":digest(raw),"icon.png":digest((self.assets/"xsoverlay-icon.png").read_bytes())}}
        self.assertEqual((self.directory/"app.json").read_bytes(),raw)
        self.assertEqual((self.directory/".lummix-install.json").read_bytes(),
                         (json.dumps(receipt,sort_keys=True)+"\n").encode())

    def test_uninstall_entry_point_removes_temporary_owned_addon(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]/"windows/runtime"))
        self.addCleanup(sys.path.pop, 0)
        import runtime_main

        self.install()
        with (patch.object(runtime_main,"wrist_sources",return_value=self.apps),
              patch.object(runtime_main,"DATA",self.assets),
              patch.object(sys,"argv",["Lummix","remove-addon"]),
              patch("sign_in.SignIn") as startup):
            runtime_main.main()
            startup.return_value.change.assert_called_once_with(False)
        self.assertEqual(self.wrist.read_bytes(),self.stock)
        self.assertFalse(self.directory.exists())

    def test_rejects_a_junction_before_touching_external_wrist(self):
        external = self.apps.parent/"outside"
        external.mkdir()
        foreign = external/"Wrist.html"
        foreign.write_bytes(self.stock)
        self.wrist.unlink()
        self.wrist.parent.rmdir()
        link = subprocess.run(["cmd.exe","/c","mklink","/J",str(self.wrist.parent),str(external)],
                              capture_output=True,timeout=5)
        if link.returncode:
            self.skipTest("Windows junction creation unavailable")
        self.addCleanup(self.wrist.parent.rmdir)
        self.assertEqual(self.manager.change("install")["state"],"conflict")
        self.assertEqual(foreign.read_bytes(),self.stock)
        self.assertFalse(self.directory.exists())


if __name__=="__main__":
    unittest.main()
