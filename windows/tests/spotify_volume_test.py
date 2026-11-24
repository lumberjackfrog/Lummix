"""Exercise guarded player volume without sending a write to Spotify or Windows audio."""
import ctypes
from ctypes import wintypes
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
import uuid

from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer
from spotify_volume_http import VolumeError, VolumeRoutes, apply_target, volume_target


class Range:
    CurrentMaximum = 1
    CurrentValue = .6
    def SetValue(self, value):
        self.CurrentValue = value


class VolumeHTTPTests(unittest.IsolatedAsyncioTestCase):
    async def test_read_bind_write_external_change_and_recreated_control(self):
        token = "a" * 43
        origin = "http://127.0.0.1:41835"

        async def check(src, key):
            return (200,None) if (src,key) == (origin,token) else (403,"refresh_selection")

        player = SimpleNamespace(validate_duck_start=check,
            tokens={token:SimpleNamespace(session=SimpleNamespace(source_app_user_model_id="Spotify.exe"))})
        control = Range()
        key = "first"
        reason = None
        def backend(edition, expected, change):
            self.assertEqual(edition,"desktop")
            if reason:
                raise VolumeError(reason)
            return apply_target((key,control,SimpleNamespace(CurrentName="Mute")),expected,change,None)

        app = web.Application()
        VolumeRoutes(player,{"spotify.exe":"desktop"},backend).install(app)
        async with TestClient(TestServer(app)) as client:
            path = "/spotify/volume"
            h = {"Origin":origin,"X-Lum-Spotify-Token":token}
            self.assertEqual((await client.get(path,headers={"Origin":origin})).status,403)
            self.assertEqual((await client.get(path,headers=dict(h,Origin="http://outside.invalid"))).status,403)
            self.assertEqual((await client.post(path,headers=h,json={"volume":10})).status,409)
            self.assertEqual((await (await client.get(path,headers=h)).json())["volume"],60)
            for invalid in ({"volume":True},{"volume":101},{"volume":-1},{"muted":0},{"muted":True,"volume":50}):
                self.assertEqual((await client.post(path,headers=h,json=invalid)).status,400)
            self.assertEqual(control.CurrentValue,.6)
            data = await (await client.post(path,headers=h,json={"volume":23.5})).json()
            self.assertEqual(data,dict(state="ready",volume=23.5,muted=False,source="spotify_player"))
            control.CurrentValue = .71
            self.assertEqual((await (await client.get(path,headers=h)).json())["volume"],71)
            key = "new-control"
            self.assertEqual((await client.post(path,headers=h,json={"volume":0})).status,409)
            self.assertEqual(control.CurrentValue,.71)
            self.assertEqual((await client.get(path,headers=h)).status,200)
            for reason in ("no_player_slider","ambiguous_player_slider","connect_device","slider_unavailable"):
                result=await client.get(path,headers=h)
                self.assertEqual(result.status,409)
                self.assertEqual(await result.json(),dict(state="unavailable",error=reason))


def chromium_probe(directory):
    import comtypes.client as com
    import psutil
    import win32gui, win32process
    mod=com.GetModule("UIAutomationCore.dll")
    ui=com.CreateObject(mod.CUIAutomation,interface=mod.IUIAutomation)
    chromium=next((Path.home()/"AppData/Local/ms-playwright").glob("chromium-*/**/chrome.exe"))
    initial="Lummix-private-60"
    fixture=directory/"fixture.html"
    fixture.write_text("""<title>Lummix-private-60</title><div role="group" aria-label="Now playing bar">
        <button id="mute">Mute</button><input aria-label="Change volume" id="vol" type="range"
        min="0" max="1" step=".01" value=".60"><input aria-label="Change progress" type="range"></div>
        <script>vol.oninput=()=>document.title='Lummix-private-'+Math.round(vol.value*100);
        mute.onclick=()=>mute.textContent=mute.textContent==='Mute'?'Unmute':'Mute';</script>""")
    command=[str(chromium),"--no-first-run","--no-default-browser-check","--disable-background-networking",
             "--disable-component-update","--disable-sync","--mute-audio","--force-renderer-accessibility",
             "--disable-extensions","--user-data-dir="+str(directory/"profile"),"--app="+fixture.as_uri()]
    browser=subprocess.Popen(command,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    hwnd=None
    try:
        # A private hosted-runner desktop can have no top-level windows while
        # Chromium is starting. FindWindowEx returns NULL for this normal case.
        find=ctypes.windll.user32.FindWindowExW
        find.argtypes=(wintypes.HWND,wintypes.HWND,wintypes.LPCWSTR,wintypes.LPCWSTR)
        find.restype=wintypes.HWND
        deadline=time.monotonic()+12
        while time.monotonic()<deadline and not hwnd:
            child=None
            while child:=find(None,child,"Chrome_WidgetWin_1",None):
                if win32process.GetWindowThreadProcessId(child)[1]==browser.pid and win32gui.GetWindowText(child).startswith(initial):
                    hwnd=child
                    break
            if not hwnd:time.sleep(.08)
        assert hwnd, "private Chromium window not ready"
        ids=[(hwnd,browser.pid,psutil.Process(browser.pid).create_time())]
        def call(key=None, change=None):
            return apply_target(volume_target(ui,mod,ids),key,change,mod)
        def page_received(title):
            deadline=time.monotonic()+1.25
            while not win32gui.GetWindowText(hwnd).startswith(title) and time.monotonic()<deadline:
                time.sleep(.025)
            return win32gui.GetWindowText(hwnd).startswith(title)
        key,answer=call()
        assert answer["volume"]==60 and answer["source"]=="spotify_player",answer
        assert call(key,{"volume":23})[1]["volume"]==23
        assert page_received("Lummix-private-23"),"page did not receive slider input"
        assert call(key,{"muted":True})[1]["muted"]
        assert not call(key,{"muted":False})[1]["muted"]
        assert call(key,{"volume":60})[1]["volume"]==60
        assert page_received(initial)
    finally:
        if hwnd: win32gui.PostMessage(hwnd,0x10,0,0)
        try: browser.wait(timeout=5)
        except subprocess.TimeoutExpired:
            browser.terminate();browser.wait(timeout=5)


class PrivateChromiumTest(unittest.TestCase):
    def test_real_chromium_range_and_event_delivery(self):
        import win32process
        import win32event
        if not any((Path.home()/"AppData/Local/ms-playwright").glob("chromium-*/**/chrome.exe")):
            self.skipTest("Private desktop Chromium fixture requires installed Playwright Chromium")
        user=ctypes.WinDLL("user32",use_last_error=True)
        user.CreateDesktopW.argtypes=(wintypes.LPCWSTR,wintypes.LPCWSTR,ctypes.c_void_p,wintypes.DWORD,wintypes.DWORD,ctypes.c_void_p)
        user.CreateDesktopW.restype=wintypes.HANDLE
        user.CloseDesktop.argtypes=[wintypes.HANDLE]
        name="LummixSliderTest"+uuid.uuid4().hex
        desk=user.CreateDesktopW(name,None,None,0,0x000F01FF,None)
        self.assertTrue(desk)
        try:
            with tempfile.TemporaryDirectory() as tmp:
                args=[sys.executable,"-B",str(Path(__file__).resolve()),"--private",tmp]
                si=win32process.STARTUPINFO();si.lpDesktop="winsta0\\"+name
                proc,thread,_,_=win32process.CreateProcess(None,subprocess.list2cmdline(args),None,None,
                    False,0x08000000,None,tmp,si)
                try:
                    self.assertEqual(win32event.WaitForSingleObject(proc,25000),0,"private timeout")
                    record=Path(tmp)/"result.json"
                    self.assertTrue(record.exists())
                    self.assertTrue(json.loads(record.read_text()).get("ok"),json.loads(record.read_text()).get("error"))
                finally:
                    thread.Close();proc.Close()
        finally:
            user.CloseDesktop(desk)


if __name__ == "__main__":
    if sys.argv[1:2] == ["--private"]:
        import traceback
        directory=Path(sys.argv[2]); result={}
        try:
            chromium_probe(directory)
            result={"ok":True}
        except Exception:
            result={"error":traceback.format_exc()}
        (directory/"result.json").write_text(json.dumps(result))
    else:
        unittest.main()


