"""Resolve artwork for an exact Now Playing link through Spotify's public oEmbed endpoint."""
import asyncio
import hashlib
import json
import time

import aiohttp
from spotify_native import NativeArtworkCache, provider


def validated_url(kind, value):
    native = provider()
    return native.artwork_url(kind, value) if native is not None else None


def spotify_item(value):
    return validated_url(0, value)


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
    item = spotify_item(item)
    if not item:
        raise ValueError("Expected a Spotify item")
    timeout = aiohttp.ClientTimeout(total=3)
    async with aiohttp.ClientSession(timeout=timeout, cookie_jar=aiohttp.DummyCookieJar()) as session:
        async with session.get("https://open.spotify.com/oembed", params={"url": item}, allow_redirects=False) as response:
            meta = json.loads(await read_limited(response, 64 * 1024))
        url = validated_url(1, meta.get("thumbnail_url")) if isinstance(meta, dict) else None
        if not url:
            raise ValueError("No Spotify thumbnail")
        async with session.get(url, allow_redirects=False) as response:
            return await read_limited(response, 4 * 1024 * 1024)


class ArtworkCache:
    """Serialize cover fetches while Rust tracks successful covers and retry deadlines."""
    def __init__(self, encode):
        self.encode = encode
        self.lock = asyncio.Lock()
        self.native_cache = None

    async def get(self, item):
        async with self.lock:
            try:
                item = spotify_item(item)
                if not item:
                    return None
                if self.native_cache is None:
                    self.native_cache = NativeArtworkCache(provider())
                status = self.native_cache.lookup(item, time.monotonic())
                if status["cached"]:
                    return tuple(status["cover"]) if status["cover"] else None
                raw = await asyncio.wait_for(fetch_cover(item), 4)
                encoded = await asyncio.to_thread(self.encode, raw)
                if encoded:
                    cover = hashlib.sha256(raw).hexdigest()[:24], encoded
                    self.native_cache.store(item, cover)
                    return cover
            except (aiohttp.ClientError, OSError, RuntimeError, ValueError, TimeoutError):
                pass
            return None
