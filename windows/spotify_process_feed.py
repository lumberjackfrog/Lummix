"""Windows' application-loopback capture, restricted to a verified Spotify process tree."""
import os
import sys
import threading


from cable_mixer import Controls
from native_audio import Engine, ProcessCapture
from spotify_cable_guard import direct_to_cable


class AmbiguousTarget(Exception):
    pass


def supported():
    return os.name=="nt" and sys.getwindowsversion().build>=20348


def spotify_target():
    from spotify_locations import spotify_process_roots
    roots = spotify_process_roots()
    if roots is None:
        raise OSError("Native Spotify discovery unavailable")
    if len(roots)>1:
        raise AmbiguousTarget()
    return roots[0] if roots else None


class SpotifyProcessFeed:
    """Only constructed for an explicitly started mix. Check routing before every attachment."""
    def __init__(self,signed_render_ids,*,engine=None,discover=spotify_target,guard=direct_to_cable):
        self.engine = engine if engine is not None else Engine(Controls())
        self.ids=frozenset(signed_render_ids)
        self.discover,self.guard=discover,guard
        self.capture=None
        self.state="connecting" if supported() else "unsupported"
        self.edition=None
        self.stop=threading.Event()
        self.thread=None
        if self.state!="unsupported":
            self.thread=threading.Thread(target=self.watch,name="LumSpotifySource",daemon=True)
            self.thread.start()

    def next(self):
        cap=self.capture
        return self.engine.read() if cap else None

    def detach(self):
        cap,self.capture=self.capture,None
        if cap:cap.close()

    def watch(self):
        try:
            while not self.stop.is_set():
                wait=.5
                try:
                    target=self.discover()
                    if self.capture and self.capture.target!=target:self.detach()
                    self.edition=target[2] if target else None
                    if not target:
                        self.state="waiting_spotify"
                    else:
                        blocked=self.guard(target[0],self.ids)
                        if blocked is not False:
                            self.detach()
                            self.state="direct_to_cable" if blocked is True else "unavailable"
                        else:
                            if self.capture is None and not self.stop.is_set():
                                self.state="connecting"
                                self.capture=ProcessCapture(self.engine, target)
                            cap=self.capture
                            status = cap.state if cap else 0
                            if status < 0 or status == 3:
                                self.detach()
                                self.state="unavailable"
                                wait=2
                            elif status in (1, 2):
                                self.state="receiving" if status == 2 else "waiting_audio"
                except AmbiguousTarget:
                    self.detach()
                    self.edition=None
                    self.state="ambiguous"
                except Exception:
                    self.detach()
                    self.state="unavailable"
                    wait=2
                self.stop.wait(wait)
        finally:
            self.detach()
            self.state="off"
            self.edition=None

    def close(self):
        self.stop.set()
        if self.thread:self.thread.join()
