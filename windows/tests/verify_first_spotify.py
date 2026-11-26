"""Exercise the published desktop setup button in a fresh, standard Windows profile."""
import ctypes
from datetime import datetime, timezone
import os
from pathlib import Path
import subprocess
import sys
import time
import traceback

from playwright.sync_api import expect, sync_playwright
from win32com.shell import shell, shellcon
from verify_sign_in import settings


def verify(folder, version):
    assert not ctypes.windll.shell32.IsUserAnAdmin(), "Spotify setup must run as a standard user"
    local = Path(shell.SHGetFolderPath(0, shellcon.CSIDL_LOCAL_APPDATA, None, 0))
    roaming = Path(shell.SHGetFolderPath(0, shellcon.CSIDL_APPDATA, None, 0))
    profile = Path(shell.SHGetFolderPath(0, shellcon.CSIDL_PROFILE, None, 0))
    os.environ.update(LOCALAPPDATA=str(local), APPDATA=str(roaming), USERPROFILE=str(profile),
                      TEMP=str(local/"Temp"), TMP=str(local/"Temp"))
    (local/"Temp").mkdir(exist_ok=True)
    assert profile.name.startswith("LumSpotifyQA"), "Disposable test account required"
    candidates = (roaming/"Spotify/Spotify.exe", local/"Spotify/Spotify.exe", local/"Programs/Spotify/Spotify.exe")
    assert not any(p.is_file() for p in candidates)
    subprocess.run([str(folder/"Lummix.Setup.exe"), "/S"], check=True, timeout=90)
    exe = local/"Programs/Lummix"/version/"Lummix.exe"
    assert exe.is_file()
    with sync_playwright() as pw:
        with settings([str(exe)], str(local), pw.chromium, return_frame=True) as frame:
            frame.get_by_role("button", name="Close settings").click()
            frame.locator("#connect-spotify").click()
            action = frame.locator("#install-player")
            expect(action).to_be_visible(timeout=30000)
            action.click()
            # No credentials, playback, VRChat or driver installation are required for this path.
            end = time.monotonic()+185
            while time.monotonic()<end and not any(p.is_file() for p in candidates):
                if frame.locator("#setup-player-help").evaluate("link=>!link.hidden"):
                    raise RuntimeError("Spotify setup reported: "+frame.locator("#player-setup-status").text_content())
                time.sleep(1)
            player = next((p for p in candidates if p.is_file()), None)
            assert player, "Spotify's website installation did not appear: "+frame.locator("#player-setup-status").text_content()
            # Spotify can finish writing after creating its executable. Wait for a valid signature.
            publisher = """
$sig=Get-AuthenticodeSignature -LiteralPath $env:LUMMIX_QA_PLAYER
if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.GetNameInfo('SimpleName',$false) -cne 'Spotify AB') { exit 1 }
"""
            until = time.monotonic()+30
            while True:
                check = subprocess.run(["powershell.exe","-NoProfile","-Command",publisher],
                                       env=dict(os.environ,LUMMIX_QA_PLAYER=str(player)),timeout=25)
                if check.returncode==0: break
                assert time.monotonic()<until, "Installed Spotify publisher could not be verified"
                time.sleep(1)
            print("The standard user's Lummix setup button installed the Spotify AB website player.",flush=True)


if __name__=="__main__":
    root,version=Path(sys.argv[1]).resolve(),sys.argv[2]
    with (root/"first-spotify.log").open("w",encoding="utf-8",buffering=1) as log:
        sys.stdout=sys.stderr=log
        try:
            print(datetime.now(timezone.utc).isoformat(),flush=True)
            verify(root,version)
        except BaseException:
            traceback.print_exc()
            sys.exit(1)
