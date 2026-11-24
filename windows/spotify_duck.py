"""Opt-in Windows Spotify ducking. Supply scalar levels from the selected dry microphone.

No capture, playback, networking, Spotify login, endpoint volume or mute API.
"""
import atexit
import ctypes
import hashlib
import json
import math
import os
from pathlib import Path, PureWindowsPath
import re
import threading
import time


def valid_depth(db):
    db = float(db)
    if not math.isfinite(db) or not 0 <= db <= 24:
        raise ValueError("Reduction must be 0 to 24 dB.")
    return db


from duck_envelope import Envelope


def spotify_path(path, env):
    path = PureWindowsPath(path)
    if path.name.lower() != "spotify.exe":
        return False
    if any(path == PureWindowsPath(env[key]) / "Spotify" / "Spotify.exe"
           for key in ("APPDATA", "LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)") if env.get(key)):
        return True
    if not env.get("ProgramFiles"):
        return False
    try:
        relative = path.relative_to(PureWindowsPath(env["ProgramFiles"]) / "WindowsApps")
    except ValueError:
        return False
    return len(relative.parts) >= 2 and bool(re.fullmatch(
        r"spotifyab\.spotifymusic_[0-9.]+_(x86|x64|arm64|neutral)__zpdnekdrzrea0",
        relative.parts[0], re.IGNORECASE))


class NativeSession:
    def __init__(self, session):
        from comtypes import GUID
        self.volume = session.channelAudioVolume()
        self.count = self.volume.GetChannelCount()
        self.context = ctypes.pointer(GUID("{59F466CB-DFC5-42A5-B792-9D94E7F75962}"))
        if not 1 <= self.count <= 32:
            raise ValueError("Unsupported channel count")

    def read(self):
        return [self.volume.GetChannelVolume(i) for i in range(self.count)]

    def write(self, values):
        for i, value in enumerate(values):
            self.volume.SetChannelVolume(i, value, self.context)


def discover_spotify():
    from comtypes import COMError
    from pycaw.pycaw import AudioSession, AudioUtilities
    from pycaw.api.audiopolicy import IAudioSessionControl2
    import psutil
    sessions = {}
    for device in AudioUtilities.GetAllDevices(data_flow=0, device_state=1):
        try:
            enumerator = device.AudioSessionManager.GetSessionEnumerator()
            count = enumerator.GetCount()
        except (COMError, OSError):
            continue
        for i in range(count):
            try:
                session = AudioSession(enumerator.GetSession(i).QueryInterface(IAudioSessionControl2))
                if not session.ProcessId or session.State == 2:
                    continue
                process = psutil.Process(session.ProcessId)
                if process.name().lower() != "spotify.exe" or not spotify_path(process.exe(), os.environ):
                    continue
                # Core Audio's session identifier persists across app restarts; include endpoint.
                name = device.id + "\n" + session.Identifier
                key = hashlib.sha256(name.encode()).hexdigest()
                sessions[key] = NativeSession(session)
            except (COMError, OSError, ValueError, psutil.Error):
                continue
    return sessions


def close_value(a, b):
    return abs(a-b) < 0.000005


class SessionGains:
    """Write-ahead journal stores only hashed session identity and channel gains.

    Accept either side of an interrupted stereo write per channel. External changes
    to balance win; the ordinary app volume slider and mute are independent.
    """
    def __init__(self, path):
        self.path = Path(path)
        self.records = json.loads(self.path.read_text()) if self.path.exists() else {}

    def save(self):
        if self.records:
            tmp = self.path.with_suffix(".tmp")
            tmp.write_text(json.dumps(self.records), encoding="utf-8")
            os.replace(tmp, self.path)
        else:
            self.path.unlink(missing_ok=True)

    def apply(self, key, session, attenuation):
        record = self.records.get(key)
        if not record and attenuation == 0:
            return
        current = session.read()
        base = list(current)
        if record and all(len(record[k]) == len(current) for k in ("base", "old", "new")):
            for i, value in enumerate(current):
                if close_value(value, record["new"][i]) or close_value(value, record["old"][i]):
                    base[i] = record["base"][i]
        gain = 10. ** (-attenuation / 20.)
        target = [v * gain for v in base]
        if any(not close_value(a, b) for a,b in zip(current,target)):
            self.records[key] = dict(base=base, old=current, new=target)
            self.save()
            session.write(target)
            self.records[key]["old"] = target
        if attenuation == 0 and key in self.records:
            del self.records[key]
            self.save()


class PersonalSpotifyDucker:
    def __init__(self):
        self._thread = None
        self._stop = threading.Event()
        self._sample = (0., None)
        self._depth = 12.
        self._status = dict(state="off", sessions=0, reductionDb=0.)

    def feed_mic_rms(self, rms):
        rms = float(rms)
        if not math.isfinite(rms) or rms < 0:
            rms = 0.
        self._sample = (min(rms, 1.), time.monotonic())

    def set_depth(self, db):
        self._depth = valid_depth(db)

    def status(self):
        return dict(self._status)

    def start_external(self, depth_db=12):
        """Call only after explicit user opt-in. Feed dry RMS every 10–40 ms."""
        self.set_depth(depth_db)
        if self._thread and self._thread.is_alive():
            return
        import comtypes  # Initialize COM on calling thread before spawning our own MTA.
        import msvcrt
        directory = Path(os.environ["LOCALAPPDATA"]) / "LumMix"
        directory.mkdir(parents=True, exist_ok=True)
        guard = (directory / "spotify-duck.lock").open("a+b")
        try:
            guard.seek(0)
            msvcrt.locking(guard.fileno(), msvcrt.LK_NBLCK, 1)
        except OSError:
            guard.close()
            raise RuntimeError("Another personal Spotify controller is active.") from None
        self._sample = (0., None)
        self._stop.clear()
        self._status = dict(state="starting", sessions=0, reductionDb=0.)
        self._thread = threading.Thread(target=self._run, args=(directory, guard), daemon=True)
        self._thread.start()
        atexit.register(self.stop)

    def _run(self, directory, guard):
        import comtypes
        sessions = {}
        failed = False
        initialized = False
        gains = None
        try:
            comtypes.CoInitializeEx(0)
            initialized = True
            gains = SessionGains(directory / "spotify-duck-recovery.json")
            env = Envelope()
            next_scan = 0.
            while not self._stop.is_set():
                now = time.monotonic()
                if now >= next_scan:
                    try:
                        sessions = discover_spotify()
                    except (comtypes.COMError, OSError):
                        sessions = {}
                    next_scan = now + 1.
                rms, stamp = self._sample
                reduction = env.step(now, rms, stamp, self._depth)
                for key, session in list(sessions.items()):
                    try:
                        gains.apply(key, session, reduction)
                    except comtypes.COMError:
                        # The process/device can vanish mid-write. Journal survives.
                        del sessions[key]
                state = ("waiting-spotify" if not sessions else
                         "waiting-mic" if stamp is None or now-stamp > .150 else "active")
                self._status = dict(state=state, sessions=len(sessions), reductionDb=round(reduction, 2))
                self._stop.wait(.025)
        except Exception:
            failed = True
        finally:
            if gains is not None:
                for key, session in sessions.items():
                    try:
                        gains.apply(key, session, 0.)
                    except Exception:
                        failed = True
            sessions.clear()
            session = None
            if initialized:
                comtypes.CoUninitialize()
            guard.close()
            self._status = dict(state="error" if failed else "off", sessions=0, reductionDb=0.)

    def stop(self):
        self._stop.set()
        if self._thread:
            self._thread.join()
        atexit.unregister(self.stop)
