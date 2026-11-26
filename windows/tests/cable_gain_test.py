"""Offline checks through the same callback used by the Windows cable stream."""
import unittest

import numpy as np

from cable_mixer import BLOCK, GAIN_RAMP, PEAK_CEILING, CableMixer, Controls


class Fixture:
    def __init__(self):
        self.mixer = CableMixer()
        self.mic = np.full((BLOCK, 1), .08, dtype=np.float32)
        self.music = np.tile(np.array([.04, -.02], dtype=np.float32), (BLOCK, 1))
        self.mixer.spotify_feed = self

    def next(self):
        return self.music

    def render(self, count=1):
        out = np.empty((count * BLOCK, 2), np.float32)
        for i in range(count):
            self.mixer._render(out[i*BLOCK:(i+1)*BLOCK], self.mic, BLOCK, None)
        return out


class GainTests(unittest.TestCase):
    def test_real_callback_negotiates_short_then_standard_device_periods(self):
        from pa_fixture import Driver
        for reject_short, reject_low, expected in (
            (False, False, [("low", 240)]),
            (True, False, [("low", 240), ("low", 480)]),
            (False, True, [("low", 240), ("low", 480), ("high", 480)]),
        ):
            with self.subTest(expected=expected):
                driver = Driver()
                driver.device.defaultLowInputLatency = driver.device.defaultLowOutputLatency = .003
                driver.reject_short, driver.reject_low = reject_short, reject_low
                mixer = CableMixer(stream_factory=driver.stream)
                mixer.start_mic_route(31, 71)
                try:
                    self.assertEqual([(m, n) for m, _, _, n, _, _ in driver.opens], expected)
                    n = mixer.stream.frames
                    voice = np.full(n, .125, np.float32)
                    out = np.empty((n, 2), np.float32)
                    driver.callback(voice.ctypes.data, out.ctypes.data, n, None, 0, driver.user)
                    np.testing.assert_array_equal(out, np.repeat(voice[:,None], 2, axis=1))
                finally:
                    mixer.stop()
                self.assertTrue(driver.closed)

    def test_independent_double_and_sample_ramps(self):
        f = Fixture()
        self.assertEqual((Controls().mic_gain, Controls().file_gain), (1, 1))
        baseline = f.render()
        np.testing.assert_allclose(baseline[0], [.12,.06], atol=1e-7)
        f.mixer.set_controls(mic_gain=2)
        raised = f.render(2)
        self.assertLess(np.abs(np.diff(np.r_[baseline[-1,0], raised[:,0]])).max(), .08/GAIN_RAMP+1e-7)
        np.testing.assert_allclose(raised[GAIN_RAMP-1:], np.tile([.20,.14], (len(raised)-GAIN_RAMP+1,1)),atol=1e-7)
        f.mixer.set_controls(file_gain=2)
        np.testing.assert_allclose(f.render(2)[-1], [.24,.12], atol=1e-7)
        f.mixer.set_controls(mute_mic=True)
        np.testing.assert_array_equal(f.render(),2 * f.music)
        f.mixer.set_controls(mute_file=True, mute_mic=False)
        np.testing.assert_array_equal(f.render(2)[-1], np.repeat(2*f.mic[-1],2))
        f.mixer.set_controls(mute_master=True)
        self.assertFalse(f.render().any())
        f.mixer.set_controls(mute_master=False, mic_gain=0, file_gain=0)
        self.assertFalse(f.render().any())
        for bad in (-1, 2.01, float("nan"), float("inf")):
            with self.assertRaises(ValueError):
                f.mixer.set_controls(file_gain=bad)

    def test_peak_protection_linked_stereo_and_recovery(self):
        f = Fixture()
        f.mic.fill(0)
        f.music[:] = [.9,-.45]
        f.mixer.set_controls(file_gain=2)
        overloaded = f.render(3)
        self.assertLessEqual(abs(overloaded).max(),np.float32(PEAK_CEILING))
        self.assertAlmostEqual(float(overloaded[-1,0]),PEAK_CEILING,places=6)
        np.testing.assert_allclose(overloaded[:,1],-.5*overloaded[:,0],atol=1e-7)
        f.music[:] = [.1,-.05]
        recovering = f.render(10)
        self.assertLess(float(recovering[0,0]),.13)
        self.assertTrue((np.diff(recovering[:,0]) >= -1e-7).all())
        np.testing.assert_array_equal(recovering[-1],2*f.music[-1])
        f.mic[:] = .99
        f.music[:] = .99
        f.mixer.set_controls(mic_gain=2)
        self.assertLessEqual(abs(f.render(2)).max(),np.float32(PEAK_CEILING))
        f.music.fill(float("nan"))
        self.assertTrue(np.isfinite(f.render()).all())

    def test_gain_change_during_ramp_and_ducked_source(self):
        f = Fixture()
        f.mic.fill(.2)
        f.music *= .25  # source already reduced upstream
        f.mixer.set_controls(mic_gain=2, file_gain=2)
        last = f.render()[-1]
        f.mixer.set_controls(mic_gain=.5)
        changed = f.render(2)
        self.assertLess(float(np.max(abs(changed[0]-last))),.001)
        np.testing.assert_allclose(changed[-1], [.12,.09], atol=1e-7)


if __name__ == "__main__":
    unittest.main()
