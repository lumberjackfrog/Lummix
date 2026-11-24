"""Static desktop assets shared by the HTTP preview and the installed WebView."""
from urllib.parse import unquote, urlsplit

STUDIO_FILES = frozenset("""
index.html Mixer%20preview.html design-board.js desktop-theme.css idle-art.js global-pulse.js global-pulse.css
studio-preferences.js live-theme.js mixer-console.css mixer-console.js mixer-settings.css mixer-visualizer.css
mixer-visualizer.js output-gains.js spotify-ducking.js transport-layout.css
visualizer-simplex-noise-4.0.3.js visualizer-simplex-noise-LICENSE.txt
""".split())


def studio_asset(folder, route):
    name = "index.html" if route == "/" else route.removeprefix("/")
    if name not in STUDIO_FILES:
        return None
    file = folder / unquote(name)
    mime = {".html": "text/html", ".css": "text/css", ".js": "text/javascript"}
    return file, mime.get(file.suffix, "text/plain")


def attach_installed_assets(host, folder):
    """Use this installation's UI even if a previous studio is serving the port."""
    import ctypes
    import comtypes
    from comtypes.gen import WebView2 as wv

    memory_stream = ctypes.WinDLL("shlwapi").SHCreateMemStream
    memory_stream.argtypes = [ctypes.c_char_p, ctypes.c_uint]
    memory_stream.restype = ctypes.POINTER(wv.IStream)

    def serve_asset(sender, args):
        request = args.Request
        target = urlsplit(request.Uri)
        if (target.scheme, target.netloc, request.Method) != ("http", "127.0.0.1:41835", "GET"):
            return 0
        asset = studio_asset(folder, target.path)
        if not asset:
            return 0
        file, mime = asset
        try:
            data = file.read_bytes()
            status, reason = 200, "OK"
        except OSError:
            data, mime, status, reason = b"Missing studio asset", "text/plain", 404, "Not Found"
        stream = memory_stream(data, len(data))
        if not stream:
            raise MemoryError("Could not load the Lummix interface.")
        headers = f"Content-Type: {mime}; charset=utf-8\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff"
        args.Response = host.environment.CreateWebResourceResponse(stream, status, reason, headers)
        return 0

    pattern = "http://127.0.0.1:41835/*"
    try:
        recent = host.web.QueryInterface(wv.ICoreWebView2_22)
    except comtypes.COMError:
        host.web.AddWebResourceRequestedFilter(pattern, wv.COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)
    else:
        recent.AddWebResourceRequestedFilterWithRequestSourceKinds(
            pattern, wv.COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            wv.COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_DOCUMENT)
    host.web.add_WebResourceRequested(host.bind(wv.ICoreWebView2WebResourceRequestedEventHandler, serve_asset))
