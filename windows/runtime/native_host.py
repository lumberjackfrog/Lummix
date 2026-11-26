"""Load the native Windows host from this installation."""
import ctypes
from functools import cache
import os
from pathlib import Path
import sys


@cache
def library():
    if getattr(sys, "frozen", False):
        path = Path(sys._MEIPASS) / "lummix_host.dll"
    elif os.environ.get("LUMMIX_HOST_DLL"):
        path = Path(os.environ["LUMMIX_HOST_DLL"]).resolve(strict=True)
    else:
        path = Path(__file__).resolve().parents[2] / ".build/host/release/lummix_host.dll"
    return ctypes.CDLL(str(path))


def wide_path(path):
    text = str(path)
    return text, len(text.encode("utf-16-le", errors="surrogatepass")) // 2
