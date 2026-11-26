"""Mic route qualification. Writes use a synthetic callback stream and temporary settings."""
import asyncio
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from pa_fixture import Driver
import numpy as np
import sounddevice as sd
from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer
from cable_mixer import CableMixer, BLOCK
from mix_gains_http import MixOutput
from mic_routes import MicRoutes, recommended


def row(id, name, kind, comm=False, default=False):
    return dict(id=id,name=name,kind=kind,headset=kind!="physical",defaultCommunications=comm,defaultInput=default)


class RouteTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        tmp=self.enterContext(TemporaryDirectory())
        self.driver=Driver()
        self.mixer=CableMixer(process_feed_factory=lambda _,**kw:None, stream_factory=self.driver.stream)
        self.output=MixOutput(Path(tmp)/"output-gains.json",self.mixer)
        self.rows=[row("phys","Physical", "physical",default=True),
                   row("vr","Pico microphone","headsetBridge",comm=True)]
        self.cables={"Render":["signed-render"],"Capture":["signed-capture"]}
        self.calls=[]
        def resolve(id,cables):
            assert id in {r["id"] for r in self.rows}
            assert cables==self.cables
            self.calls.append(("indexes",id))
            return 31,71
        self.mic=MicRoutes(self.output,inventory=lambda:self.rows,cables=lambda:self.cables,
                           headsets=lambda:"",indexes=resolve,alias=lambda ids:{"captureName":"Lummix (fixture)","captureAlias":"ready","aliasError":None})
        app=web.Application();self.output.install(app);self.mic.install(app)
        self.client=TestClient(TestServer(app));await self.client.start_server()
        self.addAsyncCleanup(self.client.close)
        self.origin={"Origin":"http://127.0.0.1:41835"}
    async def get(self):
        response=await self.client.get("/mix/mic",headers=self.origin)
        self.assertEqual(response.status,200)
        return await response.json()

    async def post(self,token,body):
        response=await self.client.post("/mix/mic",json=body,headers=self.origin|{"X-Lum-Mix-Token":token})
        return response.status,await response.json()

    async def test_end_to_end_selected_callback_gain_mute_release_without_hardware(self):
        s=await self.get();t=s["token"]
        self.assertFalse(s["routeActive"]);self.assertEqual(s["selectedId"],"phys")
        self.assertEqual((await self.post("x",{"action":"start","id":"phys"}))[0],403)
        self.assertEqual((await self.post(t,{"action":"select","id":"vr"}))[0],200)
        self.assertFalse(self.calls)
        self.assertEqual(MicRoutes(self.output).saved,"vr")
        self.assertEqual((await self.post(t,{"action":"start","id":"phys"}))[0],409)
        self.assertFalse(self.calls)
        code,active=await self.post(t,{"action":"setup","id":"vr"})
        self.assertEqual(code,200);o=active["ownerToken"]
        self.assertTrue(active["routeActive"]);self.assertEqual(active["activeId"],"vr")
        self.assertEqual(self.calls,[("indexes","vr")])
        stream=self.driver
        self.assertEqual(stream.opens,[("low",31,71,480,48000.,0)]); self.assertEqual(active["latencyMs"],{"input":11,"output":14})
        np.testing.assert_allclose(stream.generated(),.15,atol=.0001)
        self.assertAlmostEqual((await self.get())["micRms"],.15,places=4)
        h=self.origin|{"X-Lum-Mix-Token":t}
        for body,value in [({"micGain":175},.2625),({"micMuted":True},0)]:
            self.assertEqual((await self.client.post("/mix/gains",json=body,headers=h)).status,200)
            np.testing.assert_allclose(stream.generated(),value,atol=.0001)
        for action in ({"action":"start","id":"vr"},{"action":"select","id":None},
                       {"action":"release","ownerToken":"wrong"}):
            self.assertEqual((await self.post(t,action))[0],409)
        self.output.control.call("expire_token", origin=self.origin["Origin"])
        self.assertTrue((await self.get())["routeActive"])
        self.assertEqual((await self.post(t,{"action":"stop"}))[0],403)
        self.assertEqual((await self.post(t,{"action":"release","ownerToken":"wrong"}))[0],403)
        other={"Origin":"http://localhost:41835","X-Lum-Mix-Token":t}
        response=await self.client.post("/mix/mic",json={"action":"release","ownerToken":o},headers=other)
        self.assertEqual(response.status,403)
        self.assertEqual((await self.post(t,{"action":"release","ownerToken":o}))[0],200)
        self.assertTrue(stream.closed);self.assertFalse((await self.get())["routeActive"])

    async def test_disconnected_saved_device_latency_fallback_and_no_auto_resume(self):
        t=(await self.get())["token"]
        await self.post(t,{"action":"select","id":"vr"});self.driver.reject_low=True
        code,active=await self.post(t,{"action":"start","id":"vr"})
        self.assertEqual((code,active["latencyMode"]),(200,"high"))
        self.rows.pop()
        await asyncio.sleep(2.15)
        s=await self.get();self.assertEqual(s["error"],"input_disconnected")
        self.assertFalse(s["routeActive"]);self.assertEqual(s["savedId"],"vr");self.assertIsNone(s["selectedId"])
        self.assertTrue(self.driver.closed)
        self.rows.append(row("vr","renamed microphone","headsetBridge"))
        self.assertFalse((await self.get())["routeActive"])
        await self.post(t,{"action":"select","id":None})
        self.cables.clear()
        self.assertEqual((await self.post(t,{"action":"start","id":"phys"}))[1]["error"],"cable_unavailable")
        self.assertEqual(len(self.driver.opens),2)

    def test_device_recommendation_and_exclusion(self):
        self.assertEqual(recommended(self.rows,None,"pico 4"),"vr")
        self.assertEqual(recommended(self.rows,None,""),"phys")
        self.assertEqual(recommended(self.rows,"phys","pico 4"),"phys")


if __name__=="__main__":
    unittest.main()
