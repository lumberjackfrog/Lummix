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
import win32api
import win32con
import win32profile
import win32security
from verify_sign_in import mixer_frame


def verify(exe):
    assert not ctypes.windll.shell32.IsUserAnAdmin(), "Spotify setup must run as a standard user"
    # Secondary logon can inherit the runner administrator's environment even with a loaded user hive.
    token = win32security.OpenProcessToken(win32api.GetCurrentProcess(), win32con.TOKEN_QUERY | win32con.TOKEN_DUPLICATE)
    try:
        profile = Path(win32profile.GetUserProfileDirectory(token))
        account_env = win32profile.CreateEnvironmentBlock(token, False)
    finally:
        token.Close()
    assert profile.name.startswith("LumSpotifyQA"), "Disposable test account required"
    os.environ.update(account_env)
    local = Path(os.environ["LOCALAPPDATA"])
    roaming = Path(os.environ["APPDATA"])
    assert Path(os.environ["USERPROFILE"]) == profile and local.is_relative_to(profile) and roaming.is_relative_to(profile)
    (local/"Temp").mkdir(parents=True, exist_ok=True)
    os.environ.update(TEMP=str(local/"Temp"), TMP=str(local/"Temp"))
    print("Using the new standard account's own Windows profile.", flush=True)
    candidates = (roaming/"Spotify/Spotify.exe", local/"Spotify/Spotify.exe", local/"Programs/Spotify/Spotify.exe")
    assert not any(p.is_file() for p in candidates)
    assert exe.is_file()
    with sync_playwright() as pw:
        with mixer_frame([str(exe)], str(local), pw.chromium) as frame:
            frame.locator("#connect-spotify").click()
            action = frame.locator("#install-player")
            expect(action).to_be_visible(timeout=30000)
            action.click()
            print("Clicked Install Spotify desktop in the installed mixer.", flush=True)
            # No credentials, playback, VRChat or driver installation are required for this path.
            end = time.monotonic()+185
            seen = None
            while time.monotonic()<end and not any(p.is_file() for p in candidates):
                message = frame.locator("#player-setup-status").text_content()
                if seen != message:
                    print("Spotify setup:", message, flush=True)
                    seen = message
                if frame.locator("#setup-player-help").evaluate("link=>!link.hidden"):
                    raise RuntimeError("Spotify setup reported: "+message)
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
    root,exe=Path(sys.argv[1]).resolve(),Path(sys.argv[2])
    with (root/"first-spotify.log").open("w",encoding="utf-8",buffering=1) as log:
        sys.stdout=sys.stderr=log
        try:
            print(datetime.now(timezone.utc).isoformat(),flush=True)
            verify(exe)
        except BaseException:
            traceback.print_exc()
            sys.exit(1)
