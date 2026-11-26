"""Browser, HTTP, persistence and native callback checks using generated samples only."""
import asyncio
import importlib.util
import os
from pathlib import Path
import socket
import sys
import tempfile
import unittest
from unittest.mock import patch

from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer
import numpy as np
from playwright.async_api import async_playwright

from cable_mixer import BLOCK, CableMixer
from mix_gains_http import DEFAULTS, MIX_OUTPUT, MixOutput

spec = importlib.util.spec_from_file_location("mix_test_host", Path(__file__).resolve().parents[1]/"spotify-helper.py")
host = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = host
spec.loader.exec_module(host)
ORIGIN = "http://127.0.0.1:41835"


class Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name)/"output-gains.json"
        self.enterContext(patch.object(CableMixer, "start_mic_route", side_effect=AssertionError("Unexpected source/device activation")))
        self.output = MixOutput(self.path)
        self.headers = {"Origin": ORIGIN, "Host": "127.0.0.1:41838"}
        self.client = TestClient(TestServer(host.create_app(mix_output=self.output)))
        await self.client.start_server()

    async def asyncTearDown(self):
        await self.client.close()

    async def get(self):
        return await (await self.client.get("/mix/gains", headers=self.headers)).json()

    async def post(self, data, token, **options):
        headers = self.headers | {"X-Lum-Mix-Token": token} | options
        return await self.client.post("/mix/gains", json=data, headers=headers)

    async def test_guard_and_sparse_atomic_updates(self):
        first = await self.get()
        self.assertEqual({key:first[key] for key in DEFAULTS},DEFAULTS)
        self.assertFalse(first["routeActive"] or self.path.exists())
        token = first["token"]
        for body in ({}, {"musicGain":201}, {"micGain":True}, {"micGain":float("nan")},
                     {"micMuted":0}, {"musicGain":-1}, {"spotifyVolume":100}):
            self.assertEqual((await self.post(body,token)).status,400)
        self.assertEqual((await self.post({"micMuted":True},"x"*43)).status,403)
        self.assertEqual((await self.post({"micMuted":True},token,Origin="http://localhost:41835")).status,403)
        self.assertEqual((await self.post({"micMuted":True},token,Host="not-local")).status,403)
        self.assertEqual((await self.post({"micMuted":True},token,Cookie="test=1")).status,403)
        duplicate = await self.client.post("/mix/gains",data='{"micGain":10,"micGain":20}',
                         headers=self.headers|{"X-Lum-Mix-Token":token,"Content-Type":"application/json"})
        self.assertEqual(duplicate.status,400)
        self.assertFalse(self.path.exists())
        responses = await asyncio.gather(self.post({"micGain":200},token), self.post({"musicGain":0},token))
        self.assertEqual([r.status for r in responses],[200,200])
        state = await self.get()
        self.assertEqual((state["micGain"],state["musicGain"],state["revision"]), (200,0,2))
        paused=await self.post({"musicMuted":True},token)
        self.assertEqual((await paused.json())["musicGain"],0)
        with patch.object(self.output,"persist",side_effect=OSError()):
            self.assertEqual((await self.post({"micGain":4},token)).status,503)
        prefs=await self.client.post("/mix/duck",json={"depthDb":8},headers=self.headers|{"X-Lum-Mix-Token":token})
        self.assertEqual(prefs.status,200)
        restarted = MixOutput(self.path)
        async with TestClient(TestServer(host.create_app(mix_output=restarted))) as rebooted:
            saved=await (await rebooted.get("/mix/duck",headers=self.headers)).json()
            self.assertEqual((saved["enabled"],saved["depthDb"],saved["state"]),(True,8,"waiting_mic"))
            gain=await (await rebooted.get("/mix/gains",headers=self.headers)).json()
            self.assertEqual((gain["micGain"],gain["musicGain"],gain["musicMuted"]),(200,0,True))
        self.assertEqual(restarted.gains,self.output.gains)
        self.assertEqual(restarted.mixer.controls,self.output.mixer.controls)
        self.assertEqual(restarted.gains["micGain"],200)
        self.assertIsNone(restarted.mixer.stream)

    async def test_fractional_gain_roundtrips_exactly_through_native_and_disk(self):
        gain = 127.88535969157675
        token = (await self.get())["token"]
        response = await self.post({"micGain": gain}, token)
        self.assertEqual(response.status, 200)
        self.assertEqual((await response.json())["micGain"], gain)
        self.assertEqual(MixOutput(self.path).gains["micGain"], gain)

    async def test_real_browser_drives_shared_native_callback(self):
        sock=socket.socket()
        sock.bind(("127.0.0.1",0))
        address="127.0.0.1:"+str(sock.getsockname()[1])
        app=host.create_app(mix_output=self.output, expected_host=address)
        self.assertIs(app[MIX_OUTPUT].mixer,self.output.mixer)
        runner=web.AppRunner(app)
        await runner.setup()
        await web.SockSite(runner,sock).start()
        self.addAsyncCleanup(runner.cleanup)
        # Serve a real loopback document. A Playwright-fulfilled page has no actual
        # network address and Chromium correctly refuses its loopback fetch.
        frontend=web.Application()
        async def document(request):
            return web.Response(text="<title>Gain integration test</title>",content_type="text/html")
        frontend.router.add_get("/",document)
        front=web.AppRunner(frontend)
        await front.setup()
        site=web.TCPSite(front,"127.0.0.1",0)
        await site.start()
        self.addAsyncCleanup(front.cleanup)
        test_origin="http://127.0.0.1:"+str(front.addresses[0][1])
        self.enterContext(patch.object(host,"ORIGINS",host.ORIGINS|{test_origin}))
        async with async_playwright() as pw:
            browser=await pw.chromium.launch(executable_path=os.environ.get("LUMMIX_TEST_CHROMIUM"),headless=True)
            try:
                page=await browser.new_page()
                page.on("console",lambda msg: print("Chromium:",msg.text) if msg.type=="error" else None)
                page.on("requestfailed",lambda req: print("Failed fixture request",req.method,req.failure))
                await page.goto(test_origin)
                states=await page.evaluate("""async url=>{
                  const first=await (await fetch(url,{credentials:'omit'})).json();
                  const put=async fields=>{
                    const res=await fetch(url,{method:'POST',credentials:'omit',headers:{'Content-Type':'application/json','X-Lum-Mix-Token':first.token},body:JSON.stringify(fields)});
                    if(!res.ok) throw Error('Mix settings HTTP '+res.status);
                    return await res.json();
                  };
                  return [first,await put({musicGain:25}),await put({micGain:150})];
                }""","http://"+address+"/mix/gains")
            finally:
                await browser.close()
        self.assertEqual((states[-1]["micGain"],states[-1]["musicGain"],states[-1]["routeActive"]),(150,25,False))
        mic=np.full((BLOCK,1),.10,dtype=np.float32)
        stereo=np.tile(np.array([.20,.10],dtype=np.float32),(BLOCK,1))
        class FileFixture:
            def next(self):
                return stereo
            def close(self):
                pass
        def output_sample(mixer):
            mixer.spotify_feed=FileFixture()
            out=np.empty((BLOCK,2),dtype=np.float32)
            for _ in range(3):
                mixer._render(out,mic,BLOCK,None)
            return out[-1]
        for mixer in (app[MIX_OUTPUT].mixer,MixOutput(self.path).mixer):
            self.assertIsNone(mixer.stream)
            np.testing.assert_allclose(output_sample(mixer),[.20,.175],atol=1e-6)
        token=(await self.get())["token"]
        await self.post({"micMuted":True},token)
        np.testing.assert_allclose(output_sample(self.output.mixer),[.05,.025],atol=1e-6)
        await self.post({"micMuted":False,"musicMuted":True},token)
        np.testing.assert_allclose(output_sample(self.output.mixer),[.15,.15],atol=1e-6)
        print("Browser 150% mic + 25% music: native L/R 0.200/0.175; mute L/R 0.050/0.025 and 0.150/0.150.")


if __name__=="__main__":
    unittest.main()
