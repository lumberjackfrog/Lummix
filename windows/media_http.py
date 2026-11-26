"""Adapt the existing async media handlers to the native loopback listener."""
import asyncio
from contextlib import asynccontextmanager
from concurrent.futures import CancelledError
import ctypes
import json
import sys
import threading
import zlib

from aiohttp import web
from multidict import CIMultiDict
from yarl import URL


Dispatch = ctypes.CFUNCTYPE(
    ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t,
    ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_uint16),
)


class MediaRequest:
    def __init__(self, app, dispatch, body, limit=256):
        self.app = app
        self.method = dispatch["method"]
        self.path = dispatch["path"]
        self.query = URL.build(query_string=dispatch["query"], encoded=True).query
        self.headers = CIMultiDict(dispatch["headers"])
        self._dispatch = dispatch
        self._body = body
        self._limit = limit

    def clone(self, *, client_max_size):
        return MediaRequest(self.app, self._dispatch, self._body, client_max_size)

    @property
    def content_type(self):
        return self.headers.get("Content-Type", "application/octet-stream").partition(";")[0].strip().lower()

    async def json(self, *, loads=json.loads):
        def too_large(size):
            return web.HTTPRequestEntityTooLarge(max_size=self._limit, actual_size=size)

        if self._dispatch["oversized"]:
            raise too_large(self._limit + 1)
        body = self._body
        encoding = self.headers.get("Content-Encoding", "").lower()
        if encoding in ("gzip", "deflate"):
            try:
                stream = zlib.decompressobj(31 if encoding == "gzip" else 15)
                body = stream.decompress(body, self._limit + 1)
                if len(body) > self._limit:
                    raise too_large(len(body))
                if not stream.eof:
                    raise ValueError("Incomplete media request")
            except zlib.error as error:
                raise ValueError("Invalid compressed media request") from error
        if len(body) > self._limit:
            raise too_large(len(body))
        charset = "utf-8"
        for parameter in self.headers.get("Content-Type", "").split(";")[1:]:
            name, separator, value = parameter.partition("=")
            if separator and name.strip().lower() == "charset":
                charset = value.strip().strip('"')
                break
        try:
            return loads(body.decode(charset))
        except LookupError as error:
            raise ValueError("Unknown media request encoding") from error


@asynccontextmanager
async def serve(app, *, listen_port=41838, expected_host="127.0.0.1:41838", qa=False):
    if getattr(sys, "frozen", False):
        from native_host import library
    else:
        from runtime.native_host import library

    loop = asyncio.get_running_loop()
    routes = {(route.resource.canonical, route.method): route.handler for route in app.router.routes()}
    pending = {}
    lock = threading.Lock()
    closing = False

    async def dispatch(meta, body):
        request = MediaRequest(app, meta, body)
        handler = routes[request.path, request.method]
        try:
            response = await handler(request)
        except web.HTTPException as error:
            response = error
        return response.status, response.body or b""

    @Dispatch
    def exchange(meta_pointer, meta_size, body_pointer, body_size, output, capacity, status):
        future = None
        try:
            meta = json.loads(ctypes.string_at(meta_pointer, meta_size))
            body = ctypes.string_at(body_pointer, body_size)
            with lock:
                if closing:
                    return 0
                future = asyncio.run_coroutine_threadsafe(dispatch(meta, body), loop)
                pending[future] = meta['path']
            code, content = future.result()
            if len(content) > capacity:
                return 0
            if content:
                ctypes.memmove(output, content, len(content))
            status[0] = code
            return len(content)
        except CancelledError:
            return 0
        except Exception as error:
            loop.call_soon_threadsafe(loop.call_exception_handler, {
                "message": "Native media request handler failed", "exception": error,
            })
            status[0] = 500
            return 0
        finally:
            if future is not None:
                with lock:
                    pending.pop(future, None)

    dll = library()
    start = dll.lummix_media_start
    start.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_uint16, ctypes.c_bool,
                      Dispatch, ctypes.POINTER(ctypes.c_uint16)]
    start.restype = ctypes.c_void_p
    stop = dll.lummix_media_stop
    stop.argtypes = [ctypes.c_void_p]
    stop.restype = None
    payload = json.dumps({"host": expected_host, "routes": list(routes)}).encode("ascii")
    bound = ctypes.c_uint16()
    listener = start(payload, len(payload), listen_port, qa, exchange, ctypes.byref(bound))
    if not listener:
        raise OSError("The native media listener could not start.")
    try:
        yield bound.value
    finally:
        with lock:
            closing = True
            requests = list(pending.items())
        short = []
        for future, path in requests:
            if path == '/spotify/changes':
                future.cancel()
            else:
                short.append(asyncio.wrap_future(future))
        # Stop the acceptor as other requests finish. Let in-flight route commits
        # settle before the existing shutdown hooks release devices and capabilities.
        joining = asyncio.create_task(asyncio.to_thread(stop, listener))
        if short:
            _, still_running = await asyncio.wait(short, timeout=60)
            for future in still_running:
                future.cancel()
        await joining
        for shutdown in app.on_shutdown:
            await shutdown(app)


async def run(app, bind, linked):
    async with serve(app, qa=bind == "127.0.0.2"):
        if linked:
            await asyncio.to_thread(sys.stdin.buffer.read)
        else:
            await asyncio.Event().wait()
