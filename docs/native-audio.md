# Windows audio port

Lummix captures the selected microphone and the verified Spotify renderer, mixes them in Rust, and writes stereo audio to the selected VB-CABLE endpoint. The process-loopback worker, packet queue, speech detector, gain ramps, limiter and device callback run without a Python callback. The capture and render threads use a bounded queue. The capture worker checks the process creation time and discards its old queue when the selected renderer changes.

## Measurements

Measured on an AMD Ryzen 9 9950X3D running Windows x64. The baseline is commit `c8a5a2d`, which already used Rust for the inner sample mix. Both builds processed the same 48 kHz input. Each measurement covers delivering a process packet and running the complete device callback through the test driver. Each workload ran three times per build with 60,000 blocks per run. The table gives the median time per 480-frame block.

| Workload | Previous implementation | Rust capture and routing | Speedup |
| --- | ---: | ---: | ---: |
| Mic and music | 8.266 µs | 3.539 µs | 2.34× |
| Speech ducking | 8.470 µs | 3.574 µs | 2.37× |
| Peak limiting | 8.734 µs | 4.087 µs | 2.14× |

All 403,200 compared output samples matched exactly. The replay covers volume and mute changes, quiet input, split packets, overflow, discontinuities and limiting. Only the test DLL exposes packet injection.

On two dedicated Windows virtual endpoints, the new callback negotiated 240 frames and a reported output buffer of 27.04 ms. The previous callback used 480 frames and reported 32.04 ms. Both reported 22.04 ms at the input. The test observer includes the control pipe, Windows scheduling and its own capture buffer. Two alternating before/after experiments each measured 64 gain changes per build.

| Gain change at the output | Previous median | Rust median | Previous p90 | Rust p90 |
| --- | ---: | ---: | ---: | ---: |
| First device run | 65.1 ms | 66.7 ms | 80.2 ms | 74.9 ms |
| Repeat with Rust loading the driver | 77.8 ms | 66.4 ms | 83.5 ms | 84.3 ms |

All eight runs reported zero xruns. The two runs show a consistent reduction in processing time and configured output buffering. The direction of the change in observed control latency varies between runs.

The 5 ms request applies when both devices advertise low latency of at most 5 ms. Lummix retries the standard 10 ms request and then the high-latency settings if the device rejects the faster mode. Independent tone tests exercised both mutes, double gain and 12 dB of voice ducking at the Windows output. A physical headset and VRChat add their own audio buffers and network delay.

## Reproduce

- Run the manual **Lummix checks** GitHub workflow to build both revisions, compare their samples and save timing JSON in the `audio-verification` artifact.
- Run `windows/tests/device_route.py` on Windows with the named, idle Steam virtual devices to check actual capture, routing and gain. The test checks the system defaults and active sessions before opening a stream.
- Run `windows/tests/route_latency.py --baseline <previous-checkout> --output latency.json` with the same Python test environment and built release DLLs to compare device timing. The baseline checkout must include its own built DSP.
- Run `scripts/count-native-source.py --help` for an installer-backed language inventory. It checks the installer digest, inspects the frozen application and compares its shipped interface files to the source commit.
