"""Bridge application commands to the Rust-owned desktop window and WebView."""
import ctypes
from ctypes import wintypes as wt
from functools import cache
import json
import os
from pathlib import Path
import sys
from urllib.parse import urlsplit

from client_update import Updates
import diagnostics
from native_host import library, wide_path
from native_webview import WebView, READY as WEBVIEW_READY, ERROR as WEBVIEW_ERROR, MESSAGE, NEW_PAGE, KEY
import profile_save
from spotify_setup import SpotifySetup


NATIVE = "http://127.0.0.1:41835/?view=desktop&app=1"
PROJECT = Path(__file__).resolve().parent.parent
_PREFERENCE, _REMEMBER, _BEGIN_EXIT, _FINISH_EXIT, _UPDATE, _SPOTIFY, _WINDOW_FAILED = range(7)
_WINDOW_EVENT = ctypes.CFUNCTYPE(ctypes.c_int32, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_size_t)
_GEOMETRY = ("x", "y", "width", "height")


class _SavedPlacement(ctypes.Structure):
    _fields_ = [("geometry", ctypes.c_double * 4), ("screen", ctypes.c_wchar * 32),
                ("maximized", ctypes.c_int32)]


@cache
def _api():
    host = library()
    token, text, integer = ctypes.c_void_p, ctypes.c_wchar_p, ctypes.c_int32
    for name, argtypes, restype in (
        ("create", [text, text, ctypes.POINTER(ctypes.c_double), _WINDOW_EVENT], token),
        ("hwnd", [token], wt.HWND),
        ("show", [token, integer, integer], None),
        ("attach_web", [token, token], None),
        ("pump", [token], integer),
        ("close", [token], None),
        ("command", [token, text, ctypes.c_size_t], integer),
        ("accelerator", [token, ctypes.c_uint32], integer),
        ("error", [token, text, ctypes.c_size_t], None),
        ("notify", [wt.HWND, ctypes.c_uint32], None),
    ):
        function = getattr(host, "lummix_desktop_" + name)
        function.argtypes = argtypes
        function.restype = restype
    return host


def local_dir():
    return Path(os.environ["LOCALAPPDATA"]) / "LumMix"


def _window_settings(data):
    for key in ("x", "y", "width", "height"):
        if type(data.get(key)) not in (int, float) or not abs(data[key]) < 20000:
            raise ValueError("Invalid saved window dimensions")


def _load(path, validate):
    try:
        return profile_save.load(path, validate)
    except (OSError, ValueError, KeyError, TypeError):
        return {}


def _asset(name):
    if getattr(sys, "frozen", False):
        return Path(sys._MEIPASS) / name
    if name == "WebView2Loader.dll":
        return PROJECT.parent / ".build/webview2" / name
    return PROJECT / "packaging" / name


class StudioWindow:
    def __init__(self):
        self._host = _api()
        self._token = None
        self.web = None
        self.hwnd = None
        self.exiting = False
        self._placement = local_dir() / "window-placement.json"
        self.updates = Updates(local_dir(), lambda: self._notify(_UPDATE))
        self.spotify_setup = SpotifySetup(local_dir(), lambda: self._notify(_SPOTIFY))
        saved = _load(self._placement, _window_settings)
        self._maxed = saved.get("maximized") is True
        screen = saved.get("screen")
        geometry = (ctypes.c_double * 4)(*(saved.get(key, float("nan")) for key in _GEOMETRY))
        self._callback = _WINDOW_EVENT(self._event)
        self._token = self._host.lummix_desktop_create(str(_asset("Lummix.ico")),
            screen if isinstance(screen, str) else None, geometry, self._callback)
        if not self._token:
            raise RuntimeError("Windows could not create the Lummix studio window.")
        self.hwnd = self._host.lummix_desktop_hwnd(self._token)

    def _notify(self, event):
        hwnd = self.hwnd
        if hwnd and not self.exiting:
            self._host.lummix_desktop_notify(hwnd, event)

    def _event(self, event, pointer, length):
        try:
            if event == _PREFERENCE:
                from studio_preferences import checked
                data = _load(local_dir() / "studio-preferences.json", lambda row: checked(row["settings"]))
                return data.get("settings", {}).get("closeToTray") is not False
            if event == _REMEMBER:
                try:
                    if length == ctypes.sizeof(_SavedPlacement):
                        saved = _SavedPlacement.from_address(pointer)
                        profile_save.save(self._placement, dict(zip(_GEOMETRY, saved.geometry),
                            screen=saved.screen, maximized=bool(saved.maximized)))
                except OSError:
                    pass
            elif event == _BEGIN_EXIT:
                self.exiting = True
                self.updates.cancelled.set()
                self.spotify_setup.cancelled.set()
            elif event == _FINISH_EXIT:
                self.hwnd = None
                if self.web is not None:
                    self.web.close()
            elif event == _UPDATE:
                state = self.updates.status()
                if state["phase"] == "launched":
                    self._notify(_BEGIN_EXIT)
                elif self.web and self.web.source() == NATIVE:
                    self._post(state)
            elif event == _SPOTIFY and self.web and self.web.source() == NATIVE:
                self._post(self.spotify_setup.status())
            elif event == _WINDOW_FAILED:
                diagnostics.record("window_failed")
        except Exception as error:
            if self._token and event not in (_WINDOW_FAILED, _BEGIN_EXIT, _FINISH_EXIT):
                self._host.lummix_desktop_error(self._token, *wide_path(str(error)))
        return 0

    def _post(self, state):
        self.web.post_json(json.dumps(state))

    def _web_event(self, kind, source, data, detail):
        if kind == WEBVIEW_READY:
            self.updates.command("auto")
        elif kind == WEBVIEW_ERROR:
            self._host.lummix_desktop_error(self._token, *wide_path(data or "The studio could not initialize."))
        elif kind == MESSAGE:
            if not self._host.lummix_desktop_command(self._token, *wide_path(data)):
                self._app_command(source, data)
        elif kind == NEW_PAGE:
            self._open_external(data)
        elif kind == KEY:
            return self._host.lummix_desktop_accelerator(self._token, detail)
        return False

    @staticmethod
    def _open_external(uri):
        target = urlsplit(uri)
        if uri == "spotify:" or (target.scheme == "https" and target.hostname in {
            "vb-audio.com", "www.vb-audio.com", "www.spotify.com", "open.spotify.com",
        }):
            try:
                os.startfile(uri)
            except OSError:
                pass

    def _app_command(self, source, command):
        if source != NATIVE:
            return
        if command in ("spotify-setup", "spotify-setup-state") and not self.exiting:
            if command == "spotify-setup":
                self.spotify_setup.begin()
            self._post(self.spotify_setup.status())
        elif command in ("update-state", "update-check", "update-download", "update-install") and not self.exiting:
            if command != "update-state":
                self.updates.command(command.removeprefix("update-"))
            self._post(self.updates.status())
        elif command in ("logs-open", "logs-clear"):
            try:
                if command == "logs-open":
                    folder = diagnostics.folder()
                    folder.mkdir(exist_ok=True, parents=True)
                    diagnostics.request_audio_snapshot()
                    os.startfile(str(folder))
                    message = "Opened the local diagnostics folder."
                else:
                    diagnostics.clear()
                    message = "Local logs cleared."
            except OSError:
                message = "Windows couldn’t access the diagnostics folder."
            self._post({"type": "lummix-diagnostics", "message": message})
        elif command in ("startup-state", "startup-on", "startup-off"):
            from sign_in import SignIn
            startup = SignIn()
            try:
                state = startup.state() if command == "startup-state" else startup.change(command == "startup-on")
            except OSError:
                state = dict(enabled=None, editable=False, message="Lummix could not read or change Windows Startup apps.")
            self._post(dict(type="lummix-startup-state", **state))

    def run(self):
        try:
            self._host.lummix_desktop_show(self._token, self._maxed, sys.getwindowsversion().build >= 22000)
            studio_root = Path(sys.executable).parent if getattr(sys, "frozen", False) else PROJECT.parent
            self.web = WebView(self.hwnd, _asset("WebView2Loader.dll"), local_dir() / "Studio WebView",
                               studio_root / "studio", self._web_event)
            self._host.lummix_desktop_attach_web(self._token, self.web.token)
            if self._host.lummix_desktop_pump(self._token) < 0:
                raise RuntimeError("Windows could not dispatch the Lummix studio messages.")
        finally:
            self.exiting = True
            self.updates.cancelled.set()
            self.spotify_setup.cancelled.set()
            self._host.lummix_desktop_close(self._token)
            self._token = None


def run():
    StudioWindow().run()
