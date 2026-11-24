"""Native bus measurements with generated stereo audio. All host writes use a temporary profile."""
import asyncio
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
import numpy as np
from aiohttp import web
from aiohttp.test_utils import TestClient,TestServer

from cable_mixer import BLOCK,RATE,CableMixer,Controls,MixStage
from mix_duck_http import MixDucking
from mix_gains_http import MixOutput


class BusTests(unittest.TestCase):
    def test_voice_curve_stereo_music_only_and_mutes(self):
        cfg=Controls(mic_gain=.75,file_gain=1.5,duck_enabled=True,duck_depth=12)
        stage=MixStage(cfg)
        voice=np.full((BLOCK,1),.08,np.float32)
        silent=np.zeros_like(voice)
        out=np.empty((BLOCK,2),np.float32)
        music=np.tile(np.array([.16,-.08],np.float32),(BLOCK,1))
        def step(count, mic):
            for _ in range(count):
                stage.render(out,mic,music,cfg)
            return stage.duck.amount
        self.assertEqual(step(5,silent),0)
        np.testing.assert_allclose(out[-1],[.24,-.12],atol=1e-6)
        self.assertLess(step(10,voice),3)
        self.assertAlmostEqual(step(31,voice),12,places=4)
        g=10**(-12/20)
        np.testing.assert_allclose(out[-1],[.06+.24*g,.06-.12*g],atol=1e-5)
        self.assertAlmostEqual(step(44,silent),12,places=5)
        self.assertAlmostEqual(step(51,silent),6,delta=.2)
        self.assertAlmostEqual(step(51,silent),0,places=5)
        np.testing.assert_allclose(out[-1],[.24,-.12],atol=1e-6)
        from dataclasses import replace
        cfg=replace(cfg,mute_mic=True)
        self.assertEqual(step(48,voice),0)
        np.testing.assert_allclose(out[-1],[.24,-.12],atol=1e-6)
        self.assertEqual(cfg.file_gain,1.5)



class ApiTests(unittest.IsolatedAsyncioTestCase):
    async def test_saved_passive_state_and_guard(self):
        folder=self.enterContext(tempfile.TemporaryDirectory())
        mixer=CableMixer()
        mix=MixOutput(Path(folder)/"output-gains.json",mixer)
        mic=SimpleNamespace(active=None)
        api=MixDucking(mix,mic);app=web.Application();api.install(app)
        h={"Origin":"http://127.0.0.1:41835"}
        async with TestClient(TestServer(app)) as client:
            s=await (await client.get("/mix/duck",headers=h)).json()
            self.assertEqual((s["state"],s["enabled"],s["depthDb"]),("waiting_mic",True,12))
            self.assertFalse(s["configured"])
            self.assertIsNone(mixer.stream)
            self.assertEqual((await client.post("/mix/duck",headers=h,json={"enabled":False})).status,403)
            h["X-Lum-Mix-Token"]=s["token"]
            for bad in ({"depthDb":0},{"enabled":1},{"depthDb":25},{"rms":.1}):
                self.assertEqual((await client.post("/mix/duck",headers=h,json=bad)).status,400)
            for pref in ({"depthDb":9,"migrate":True},{"enabled":False},{"depthDb":23,"migrate":True}):
                self.assertEqual((await client.post("/mix/duck",headers=h,json=pref)).status,200)
            self.assertEqual(MixDucking(mix,mic).settings,{"enabled":False,"depthDb":9})
            self.assertFalse(mixer.controls.duck_enabled)
            self.assertIsNone(mixer.stream)
            await client.post("/mix/duck",headers=h,json={"enabled":True})
            mic.active="fixture"
            mixer.stream=SimpleNamespace(active=True)
            self.assertEqual(api.snapshot("")["state"],"waiting_music")
            mixer._stage.render(np.empty((BLOCK,2),np.float32),np.ones((BLOCK,1),np.float32)*.1,np.full((BLOCK,2),.2,np.float32),mixer.controls)
            self.assertEqual(api.snapshot("")["state"],"ready")
            mixer.set_controls(mute_mic=True)
            self.assertEqual(api.snapshot("")["state"],"mic_muted")
            self.assertEqual(mix.gains["musicGain"],100)


if __name__=="__main__":
    unittest.main()
