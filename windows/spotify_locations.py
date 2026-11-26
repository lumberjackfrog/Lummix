"""Discover each Windows user's Spotify installations and remember the last valid locations."""
import os
from itertools import chain
from pathlib import Path, PureWindowsPath
import re

import profile_save


STORE_FAMILY = "SpotifyAB.SpotifyMusic_zpdnekdrzrea0"


def desktop_paths(env):
    for key in ("APPDATA", "LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"):
        if env.get(key):
            yield PureWindowsPath(env[key]) / "Spotify" / "Spotify.exe"
    if env.get("LOCALAPPDATA"):
        yield PureWindowsPath(env["LOCALAPPDATA"]) / "Programs/Spotify/Spotify.exe"


def local_executable(candidate):
    path = PureWindowsPath(candidate)
    return (path.is_absolute() and re.fullmatch(r"[a-zA-Z]:", path.drive) is not None
            and path.name.casefold() == "spotify.exe" and ".." not in path.parts)


def registered_desktop_paths():
    import winreg
    app_paths = r"Software\Microsoft\Windows\CurrentVersion\App Paths\Spotify.exe"
    uninstall = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Spotify"
    for hive in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE):
        for view in (winreg.KEY_WOW64_64KEY, winreg.KEY_WOW64_32KEY):
            for key, field in ((app_paths, ""), (uninstall, "InstallLocation"), (uninstall, "DisplayIcon")):
                try:
                    with winreg.OpenKey(hive, key, 0, winreg.KEY_READ | view) as handle:
                        value, _ = winreg.QueryValueEx(handle, field)
                except (FileNotFoundError, PermissionError):
                    continue
                if not isinstance(value, str) or not value.strip():
                    continue
                if field == "DisplayIcon":
                    value = re.sub(r",\s*-?\d+\s*$", "", value)
                path = PureWindowsPath(os.path.expandvars(value.strip().strip('"')))
                if field == "InstallLocation":
                    path /= "Spotify.exe"
                if local_executable(path) and "windowsapps" not in (part.casefold() for part in path.parts):
                    yield path


def store_packages():
    from winsdk.windows.management.deployment import PackageManager, PackageTypes
    packages = PackageManager().find_packages_for_user_with_package_types("", STORE_FAMILY, PackageTypes.MAIN)
    return list(packages or ())


def _store_paths(packages):
    for package in packages:
        try:
            install_path = package.installed_path
        except (AttributeError, NotImplementedError, OSError, RuntimeError):
            install_path = ""
        try:
            if not install_path:
                folder = package.installed_location
                install_path = folder.path if folder else ""
            if install_path:
                candidate = PureWindowsPath(install_path) / "Spotify.exe"
                if local_executable(candidate):
                    yield candidate
        except (AttributeError, OSError, RuntimeError):
            continue


def known_process_path(candidate, env):
    path = PureWindowsPath(candidate)
    if not local_executable(path):
        return False
    if path in desktop_paths(env):
        return True
    if "windowsapps" in (part.casefold() for part in path.parts):
        if len(path.parts) < 4 or path.parent.parent.name.casefold() != "windowsapps":
            return False
        if not re.fullmatch(r"SpotifyAB\.SpotifyMusic_[0-9.]+_(x86|x64|arm64|neutral)__zpdnekdrzrea0",
                            path.parent.name, re.IGNORECASE):
            return False
        if env.get("ProgramFiles") and path.parent.parent == PureWindowsPath(env["ProgramFiles"]) / "WindowsApps":
            return True
        try:
            return env is os.environ and path in _store_paths(store_packages())
        except (AttributeError, ImportError, OSError, RuntimeError):
            return False
    return env is os.environ and path in registered_desktop_paths()


def validate_locations(data):
    if set(data) - {"store", "desktop"} or any(not isinstance(p, str) or not local_executable(p) for p in data.values()):
        raise ValueError("Invalid Spotify installation location")


def _file():
    return Path(os.environ.get("LOCALAPPDATA") or Path.home()) / "Lummix" / "spotify-locations.json"


def _saved():
    try:
        return profile_save.load(_file(), validate_locations)
    except (OSError, ValueError, KeyError, TypeError):
        return {}


def _remember(edition, path):
    try:
        profile_save.update(_file(), {edition: str(path)}, validate_locations)
    except OSError:
        pass  # A read-only profile must still discover an installed player.


def website_executable():
    saved = _saved()
    regular = list(desktop_paths(os.environ))
    # A settings file is a hint. Only use an executable in a known installation or registered location.
    old = PureWindowsPath(saved["desktop"]) if "desktop" in saved else None
    if old in regular:
        regular.remove(old)
        regular.insert(0, old)
    for path in chain(regular, registered_desktop_paths()):
        if not local_executable(path):
            continue
        try:
            present = Path(path).is_file()
        except OSError:
            continue
        if present:
            _remember("desktop", path)
            return Path(path)
    return None


def website_installed():
    return website_executable() is not None


def store_installed():
    packages = store_packages()
    registered = None
    for candidate in _store_paths(packages):
        try:
            if Path(candidate).is_file():
                _remember("store", candidate)
                return True
        except PermissionError:
            # The main package is registered even if Windows restricts access to its files.
            if registered is None:
                registered = candidate
        except OSError:
            continue
    if registered is not None:
        _remember("store", registered)
    return bool(packages)
