"""Loopback control surface for the single trusted Lum Mix preview origin."""
import hmac
import json
import secrets
import threading
import time
import cable_setup
from cable_routes import endpoint_names

ORIGIN="http://127.0.0.1:41835"
HOST="127.0.0.1:41836"
_guard=threading.Lock()
_token=None
_used=None
_issued=0.0


def permitted(handler):
    return handler.headers.get_all("Host")==[HOST] and handler.headers.get_all("Origin")==[ORIGIN]


def api(handler, method):
    global _token,_used,_issued
    if not permitted(handler):
        handler.send_error(403);return
    if method=="OPTIONS":
        custom={h.strip().lower() for h in handler.headers.get("Access-Control-Request-Headers","").split(",")}
        if handler.headers.get("Access-Control-Request-Method")!="POST" or custom!={"content-type","x-lum-setup-token"}:
            handler.send_error(403);return
        handler.send_response(204)
        handler.send_header("Access-Control-Allow-Origin",ORIGIN)
        handler.send_header("Access-Control-Allow-Methods","POST")
        handler.send_header("Access-Control-Allow-Headers","Content-Type, X-Lum-Setup-Token")
        handler.send_header("Vary","Origin, Access-Control-Request-Method, Access-Control-Request-Headers")
        handler.send_header("Cache-Control","no-store")
        handler.end_headers();return
    with _guard:
        now=time.monotonic()
        if method=="GET":
            if not _token or now-_issued>300 or _token==_used:
                _token=secrets.token_urlsafe(32);_issued=now
            data=cable_setup.status()
        else:
            provided=handler.headers.get_all("X-Lum-Setup-Token") or []
            if (len(provided)!=1 or not _token or now-_issued>300
                    or not provided[0].isascii() or not hmac.compare_digest(_token,provided[0])):
                handler.send_error(403);return
            if handler.headers.get_content_type()!="application/json" or handler.headers.get("Transfer-Encoding"):
                handler.send_error(415);return
            sizes=handler.headers.get_all("Content-Length") or []
            if len(sizes)!=1 or not sizes[0].isdigit() or not 2<=int(sizes[0])<=32:
                handler.send_error(400);return
            try:
                if json.loads(handler.rfile.read(int(sizes[0])))!={}:
                    raise ValueError
            except (ValueError,UnicodeError):
                handler.send_error(400);return
            data=cable_setup.status() if _token==_used else cable_setup.begin()
            _used=_token
        data=dict(data,token=_token)
        data["endpoints"]=None
        if data["ready"]:
            try:
                data["endpoints"]=endpoint_names()
            except Exception:
                pass  # The separately verified install state remains valid if name lookup fails.
    payload=json.dumps(data,ensure_ascii=True).encode()
    handler.send_response(202 if method=="POST" and data["state"] in cable_setup.BUSY else 200)
    for k,v in {"Content-Type":"application/json","Content-Length":str(len(payload)),"Access-Control-Allow-Origin":ORIGIN,
                "Cache-Control":"no-store","Vary":"Origin","X-Content-Type-Options":"nosniff"}.items():
        handler.send_header(k,v)
    handler.end_headers();handler.wfile.write(payload)
