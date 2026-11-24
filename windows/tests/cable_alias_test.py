"""Alias qualification. All write-path tests are simulated; the live test only reads the signed endpoint."""
import unittest
from unittest.mock import patch
import cable_alias as a
from mic_routes import signed_cables
from cable_routes import signed_names


class AliasTests(unittest.TestCase):
    def test_exact_post_action_readback_and_second_call_no_dialog(self):
        old=dict(id="verified",name="Native name (Vendor)",short="Native name",adapter="Vendor",collision=False)
        renamed=dict(old,name="Lummix (Vendor)",short="Lummix")
        with patch.object(a,"endpoint",side_effect=[old,renamed,renamed]),patch("endpoint_private_desktop.private_run",return_value=0) as ui:
            first=a.ensure({"Capture":["verified"]})
            second=a.ensure({"Capture":["verified"]})
        self.assertEqual(first,second);self.assertEqual(first,dict(captureName="Lummix (Vendor)",captureAlias="ready",aliasError=None))
        self.assertEqual(ui.call_count,1)
        self.assertEqual(ui.call_args.args[1][-1],"verified")

    def test_collision_unverified_or_missing_leave_device_alone(self):
        old=dict(id="a",name="Physical (Vendor)",short="Physical",adapter="Vendor",collision=True)
        with patch.object(a,"endpoint",return_value=old),patch("endpoint_private_desktop.private_run") as ui:
            self.assertEqual(a.ensure({})["aliasError"],"name_collision");ui.assert_not_called()
        with patch("endpoint_private_desktop.private_run") as ui:
            for invalid in ({"Capture":[]},{"Capture":["{0.0.0.00000000}.{render}"],"Render":["x"]}):
                self.assertEqual(a.ensure(invalid)["captureAlias"],"unavailable")
            ui.assert_not_called()

    def test_native_readback_without_any_open_or_write(self):
        ids=signed_cables()
        if len(ids.get("Capture",[]))!=1:self.skipTest("no unique signed VB-CABLE")
        info=a.endpoint(ids)
        self.assertEqual(info["id"],ids["Capture"][0].lower())
        self.assertEqual(info["name"],signed_names(ids)["capture"])
        self.assertEqual(info["name"],ids["captureName"])
        self.assertTrue(info["name"].startswith(info["short"]))


if __name__=="__main__":unittest.main()
