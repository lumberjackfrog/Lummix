"""Resolve artwork for an exact Now Playing link through Spotify's public oEmbed endpoint."""
import asyncio
import hashlib
import json
import re
import time

import aiohttp


def spotify_item(value):
    if isinstance(value, str):
        match = re.fullmatch(r"https://(?:xpui\.app|open)\.spotify\.com/(album|track|episode)/([A-Za-z0-9]{22})(?:\?[^#\s]*)?", value)
        if match:
            return "https://open.spotify.com/" + "/".join(match.groups())
    return None


async def read_limited(response, limit):
    response.raise_for_status()
    if response.status != 200 or response.content_length is not None and response.content_length > limit:
        raise ValueError("Unexpected Spotify response")
    data = bytearray()
    async for block in response.content.iter_chunked(32768):
        if len(data) + len(block) > limit:
            raise ValueError("Spotify response too large")
        data.extend(block)
    return bytes(data)


async def fetch_cover(item):
    timeout = aiohttp.ClientTimeout(total=3)
    async with aiohttp.ClientSession(timeout=timeout, cookie_jar=aiohttp.DummyCookieJar()) as session:
        async with session.get("https://open.spotify.com/oembed", params={"url": item}, allow_redirects=False) as response:
            meta = json.loads(await read_limited(response, 64 * 1024))
        url = meta.get("thumbnail_url") if isinstance(meta, dict) else None
        if not isinstance(url, str) or not re.fullmatch(
                r"https://(?:i\.scdn\.co|image-cdn-[a-z0-9-]+\.spotifycdn\.com)/image/[a-zA-Z0-9]+", url):
            raise ValueError("No Spotify thumbnail")
        async with session.get(url, allow_redirects=False) as response:
            return await read_limited(response, 4 * 1024 * 1024)


class ArtworkCache:
    """One cover in memory. Retry transient failures without refetching on each status poll."""
    def __init__(self, encode):
        self.encode = encode
        self.lock = asyncio.Lock()
        self.item = self.cover = None
        self.retry_at = 0

    async def get(self, item):
        item = spotify_item(item)
        if not item:
            return None
        async with self.lock:
            if self.item == item and (self.cover or time.monotonic() < self.retry_at):
                return self.cover
            self.item, self.cover = item, None
            self.retry_at = time.monotonic() + 20
            try:
                raw = await asyncio.wait_for(fetch_cover(item), 4)
                encoded = await asyncio.to_thread(self.encode, raw)
                if encoded:
                    self.cover = hashlib.sha256(raw).hexdigest()[:24], encoded
            except (aiohttp.ClientError, OSError, ValueError, TimeoutError):
                pass
            return self.cover
