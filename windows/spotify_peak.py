"""Read the WASAPI peak scalar for verified local Spotify render sessions."""
import asyncio
from concurrent.futures import ThreadPoolExecutor
import time

from spotify_locations import spotify_processes
from spotify_native import provider as media_provider


class SpotifyPeak:
    def __init__(self):
        self.worker = ThreadPoolExecutor(max_workers=1, thread_name_prefix="SpotifyPeak")
        self.processes = []
        self.edition = None
        self.refresh = self.sampled = 0
        self.value = None
        self.lock = asyncio.Lock()

    def read_native(self, edition):
        now = time.monotonic()
        try:
            native = media_provider()
            if native is None:
                return None
            if self.edition != edition or now >= self.refresh:
                self.processes = [(pid, created) for pid, (_, created, actual) in spotify_processes().items()
                                  if actual == edition]
                self.edition, self.refresh = edition, now + 2
            self.value = native.peak(self.processes)
        except (OSError, RuntimeError):
            self.value = None
        self.sampled = now
        return self.value

    async def read(self, edition):
        async with self.lock:
            if edition == self.edition and time.monotonic() - self.sampled < .15:
                return self.value
            return await asyncio.get_running_loop().run_in_executor(self.worker, self.read_native, edition)

    async def close(self, app):
        await asyncio.to_thread(self.worker.shutdown, wait=True, cancel_futures=True)
