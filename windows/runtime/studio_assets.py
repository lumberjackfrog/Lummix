"""Static desktop assets shared by the HTTP preview and the installed WebView."""
from pathlib import Path
import sys
from urllib.parse import unquote

_ASSET_DIR = Path(getattr(sys, "_MEIPASS", Path(__file__).resolve().parent))
STUDIO_FILES = frozenset((_ASSET_DIR / "studio-assets.txt").read_text(encoding="ascii").split())


def studio_asset(folder, route):
    name = "index.html" if route == "/" else route.removeprefix("/")
    if name not in STUDIO_FILES:
        return None
    file = folder / unquote(name)
    mime = {".html": "text/html", ".css": "text/css", ".js": "text/javascript", ".wasm": "application/wasm"}
    return file, mime.get(file.suffix, "text/plain")
