"""Python window callbacks for the Rust-owned Edge WebView2 controller."""
import ctypes
from ctypes import wintypes as wt
from functools import cache

from native_host import library


READY, ERROR, MESSAGE, NEW_PAGE, KEY = range(5)
_CALLBACK = ctypes.CFUNCTYPE(ctypes.c_int32, ctypes.c_uint32,
                             ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_uint32)


@cache
def _api():
    host = library()
    token, text, integer = ctypes.c_void_p, ctypes.c_wchar_p, ctypes.c_int32
    for name, argtypes, restype in (
        ("start", [wt.HWND, text, text, text, _CALLBACK, ctypes.POINTER(token)], integer),
        ("close", [token], None),
        ("post_json", [token, text], integer),
        ("source", [token, ctypes.POINTER(ctypes.c_wchar), ctypes.c_size_t], ctypes.c_size_t),
    ):
        function = getattr(host, "lummix_webview_" + name)
        function.argtypes = argtypes
        function.restype = restype
    return host


def _checked(status):
    if status < 0:
        raise OSError(f"Microsoft Edge WebView2 returned HRESULT 0x{status & 0xffffffff:08X}.")


class WebView:
    def __init__(self, hwnd, loader, profile, assets, callback):
        self.host = _api()
        self.token = ctypes.c_void_p()
        self.ready = False

        @_CALLBACK
        def events(kind, source, data, detail):
            try:
                if kind == READY:
                    self.ready = True
                return bool(callback(kind, source, data, detail))
            except Exception as error:
                callback(ERROR, None, str(error), 0)
                return False

        self.events = events
        status = self.host.lummix_webview_start(hwnd, str(loader), str(profile), str(assets),
                                               events, ctypes.byref(self.token))
        if status < 0:
            raise RuntimeError("Microsoft Edge WebView2 Runtime could not start. Install the Evergreen x64 runtime from Microsoft and reopen Lummix.")

    def __bool__(self):
        return self.ready and bool(self.token)

    def close(self):
        if self.token:
            token, self.token = self.token, ctypes.c_void_p()
            self.ready = False
            self.host.lummix_webview_close(token)
        self.events = None

    def post_json(self, payload):
        _checked(self.host.lummix_webview_post_json(self.token, payload))

    def source(self):
        size = self.host.lummix_webview_source(self.token, None, 0)
        if not size:
            return ""
        value = ctypes.create_unicode_buffer(size)
        return value.value if self.host.lummix_webview_source(self.token, value, size) == size else ""
