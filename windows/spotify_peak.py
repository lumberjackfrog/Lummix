"""Read the WASAPI peak scalar for verified local Spotify render sessions."""
import asyncio
from concurrent.futures import ThreadPoolExecutor
import math
import os
from pathlib import PureWindowsPath
import time

from spotify_duck import spotify_path


def discover_meters(edition):
    from comtypes import COMError
    from pycaw.api.audiopolicy import IAudioSessionControl2
    from pycaw.api.endpointvolume import IAudioMeterInformation
    from pycaw.pycaw import AudioUtilities
    import psutil

    meters = []
    for device in AudioUtilities.GetAllDevices(data_flow=0, device_state=1):
        try:
            sessions = device.AudioSessionManager.GetSessionEnumerator()
            count = sessions.GetCount()
        except (COMError, OSError):
            continue
        for index in range(count):
            try:
                control = sessions.GetSession(index).QueryInterface(IAudioSessionControl2)
                pid = control.GetProcessId()
                if not pid or control.GetState() != 1:
                    continue
                path = PureWindowsPath(psutil.Process(pid).exe())
                actual = "store" if "windowsapps" in (part.casefold() for part in path.parts) else "desktop"
                if actual == edition and spotify_path(path, os.environ):
                    meters.append(control.QueryInterface(IAudioMeterInformation))
            except (COMError, OSError, psutil.Error):
                continue
    return meters


class SpotifyPeak:
    def __init__(self):
        self.worker = ThreadPoolExecutor(max_workers=1, thread_name_prefix="SpotifyPeak")
        self.meters = []
        self.edition = None
        self.refresh = self.sampled = 0
        self.value = None
        self.initialized = False
        self.lock = asyncio.Lock()

    def read_native(self, edition):
        import comtypes
        now = time.monotonic()
        if self.edition != edition or now >= self.refresh:
            if not self.initialized:
                comtypes.CoInitialize()
                self.initialized = True
            try:
                self.meters = discover_meters(edition)
            except (comtypes.COMError, OSError):
                self.meters = []
            self.edition = edition
            self.refresh = now + 2
        valid = []
        for meter in self.meters:
            try:
                peak = meter.GetPeakValue()
            except (comtypes.COMError, OSError):
                continue
            if math.isfinite(peak) and 0 <= peak <= 1:
                valid.append(peak)
        self.value = max(valid, default=None)
        self.sampled = now
        return self.value

    async def read(self, edition):
        async with self.lock:
            if edition == self.edition and time.monotonic() - self.sampled < .15:
                return self.value
            return await asyncio.get_running_loop().run_in_executor(self.worker, self.read_native, edition)

    def release_native(self):
        self.meters.clear()
        if self.initialized:
            from comtypes import CoUninitialize
            CoUninitialize()

    async def close(self, app):
        await asyncio.get_running_loop().run_in_executor(self.worker, self.release_native)
        self.worker.shutdown(wait=False)
