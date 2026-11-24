"""Speech gating and real-time gain-reduction envelope, in decibels."""
class Envelope:
    def __init__(self):
        self.amount = 0.0
        self.on_since = self.last_voice = self.last_tick = None
        self.open = False
        self.target = self.rate = 0.

    def step(self, now, rms, sample_time, depth):
        fresh = sample_time is not None and 0 <= now - sample_time <= .150
        voiced = fresh and rms >= (0.004 if self.open else 0.01)
        if voiced:
            if self.on_since is None:
                self.on_since = sample_time
            if sample_time - self.on_since >= .035:
                self.open = True
                self.last_voice = sample_time
        else:
            self.on_since = None
        if self.last_voice is None or now - self.last_voice >= .450 or not fresh:
            self.open = False
        target = depth if self.open else 0.
        dt = min(.080, max(0., now - self.last_tick)) if self.last_tick is not None else 0.
        self.last_tick = now
        # Equal dB steps sound smooth. Full-depth transitions take 350/1000 ms.
        if target != self.target:
            duration = .350 if target > self.amount else 1.000
            self.rate = abs(target - self.amount) / duration
            self.target = target
        if target > self.amount:
            self.amount = min(target, self.amount + dt * self.rate)
        else:
            self.amount = max(target, self.amount - dt * self.rate)
        return self.amount
