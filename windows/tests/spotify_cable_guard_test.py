"""The music route must never feed a second copy into the same cable."""
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import spotify_cable_guard as guard


class GuardTests(unittest.TestCase):
    def route(self, cable, headphones):
        ids = {"cable-render-guid"}
        rows = [SimpleNamespace(GetId=lambda: "CABLE-RENDER-GUID", states=cable),
                SimpleNamespace(GetId=lambda: "headphones-guid", states=headphones)]
        with patch.object(guard, "session_state", side_effect=lambda dev, pids: dev.states):
            return guard.inspect_routes(rows, ids, {100,101})

    def test_cable_wins_even_when_both_outputs_are_active(self):
        self.assertIs(self.route((True,False),(True,False)), True)

    def test_stale_entry_clears_on_known_headphone_playback(self):
        self.assertIs(self.route((False,True),(True,False)), False)
        self.assertIsNone(self.route((False,True),(False,False)))

    def test_missing_verified_device_withholds_music(self):
        self.assertIsNone(guard.inspect_routes([], {"absent-device"}, {100}))
        self.assertIsNone(guard.direct_to_cable(100, []))

    def test_regular_render_path(self):
        self.assertIs(self.route((False,False),(True,False)), False)


if __name__ == "__main__":
    unittest.main()
