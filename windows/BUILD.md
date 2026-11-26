# Build Lummix on Windows

Use a 64-bit Windows development machine with CPython 3.12 x64 (including the `py` launcher), and NSIS 3. The MSI builder uses Python 3.12's `msilib`. Desktop playback uses the installed Evergreen WebView2 runtime. Application-specific audio capture needs Windows build 20348 or newer.

Install rustup and the x64 C++ linker from Visual Studio Build Tools on the build machine. The repository pins the Rust compiler and WebAssembly target. The installer includes the compiled Windows audio, media and host modules and the shared WebAssembly controls. Friends only need the Windows installer.

From the repository root, run:

```powershell
.\scripts\build.ps1
```

The script creates a repository-local virtual environment for build tooling, obtains the pinned PortAudio binary, stages the Rust executable and assets, and builds **dist/Lummix Setup.exe**. MSI and ZIP intermediates are in `dist/`. The shipping package runs without Python. The source tree requires no personal account credentials or installed audio driver to build. If NSIS is in a custom location, pass `-MakeNsis 'C:\path to NSIS\makensis.exe'`. Use `-MsiOnly` to build the intermediates. Supply `-Python` with the path to an existing x64 CPython 3.12 **virtual environment** to reuse its interpreter.

## Source and dependencies

- `studio/` contains the desktop interface. The shared asset list is `windows/runtime/studio-assets.txt`. The browser bindings load the module built from `windows/controls-rs/` on desktop and wrist.
- `windows/dsp/` owns the live microphone input, process-loopback capture, bounded audio queue, gain ramps, speech ducking, limiting and duplex device callback. `windows/media_native/` identifies the Spotify process, checks its output route and supplies playback events, controls and artwork. Audio starts on an explicit mixer action.
- `windows/host-rs/` contains native host services, the WebView2 controller and the XSOverlay integration. `windows/runtime/build_runtime.py` stages `Lummix.exe`, native dependencies and resources. `windows/packaging/` builds the ZIP and per-user MSI. `setup/` wraps the MSI into the one-file installer.
- Version 1.0.9 obtains base VB-CABLE Pack46 on the user's PC only when first-time cable installation is requested. Setup checks the archive's exact 1,324,680 bytes and SHA-256 `bc44e8974191a5a2427d6c979e2dab08dbb09925f3f435dfa2f0ba3e45b6eb90`, all 31 members, the vendor's Authenticode signer and the Microsoft driver catalog before requesting elevation. The driver is distributed by VB-Audio. An existing working cable can be reused offline. [Original VB-CABLE package](https://vb-audio.com/Cable/) · [Distribution terms](https://vb-audio.com/Services/licensing.htm)
- The compiler downloads Microsoft.Web.WebView2 1.0.4370.26 from the official NuGet feed, verifies SHA-256 `1c0dacac84b70109b3fd42f90c53a0ba942abc3a87590d7a4e0ccd86e4a06cf4`, then checks the x64 loader's hash and Microsoft Authenticode identity. Only the loader and notices go into the package. [Microsoft SDK package](https://www.nuget.org/packages/Microsoft.Web.WebView2/1.0.4370.26) · [WebView2 runtime distribution](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution)
- `.build/` stores verified downloads, compiled Rust targets, the SDK loader and the staged native runtime. `dist/` stores generated distributables. XSOverlay and SteamVR resources come from the user's own local installations. [Third-party notices](../THIRD_PARTY.md)

## Tests

`scripts/test-windows.ps1` builds and tests the locked Rust crates and exercises the deployed interfaces. The manual GitHub checks also replay generated audio against two fixed revisions. The comparison with `c8a5a2d` measures process packet delivery through the real output callback and compares the resulting samples. Reproduce it with `windows/tests/route_replay.py --help`.

On a Windows machine with the dedicated Steam virtual audio endpoints, `windows/tests/device_route.py` tests real WASAPI capture, routing, gain and ducking. `windows/tests/route_latency.py` measures a gain change from a test control pipe to a process-loopback observer. Both tests check that their endpoints are idle and use generated signals. The results include Windows buffering. An in-game microphone-to-listener measurement requires a separate VRChat receiver.

Measured timings and the reproduction steps are in the [Windows audio comparison](../docs/native-audio.md).

The backend fixtures cover source selection, authorization, route ownership, gain and limiting, sidechain ducking, media transport and installer outcomes. The browser-to-native gain test needs Chromium. Install its test dependency once after creating the build environment:

```powershell
.\.venv\Scripts\python.exe -m pip install -r .\windows\requirements-test.txt
.\.venv\Scripts\python.exe -m playwright install chromium
.\scripts\test-windows.ps1
```

Set `LUMMIX_TEST_CHROMIUM` to a compatible local Chrome/Chromium executable to use an existing browser for the gain test. The source tests use generated signals and disposable settings. The cable identity test reads the signed endpoint when available. The optional `-Frozen` check runs the actual packaged services on a second loopback address, reads device and Spotify status, verifies vendor signatures and waits for all three helpers to exit.

Use a clean, interactive Windows test account for `windows/tests/verify_setup_exe.py`. It installs a per-user Windows Installer product and checks both the desktop and Start menu shortcuts. The published installer smoke test also deletes the desktop shortcut and runs Setup again to verify repair. `verify_window.py` and `verify_tray.py` exercise Windows/WebView2 window and shell behavior.

Unsigned local builds may be refused by an organization's Windows Application Control policy. A build hash proves the artifact's identity; a successful install and launch on the target policy is a separate release check. Keep code-signing and release approval with the publisher. Build tools, local paths and MSI package identifiers can affect the output hash.

For a signed release, supply `-SigningThumbprint` for a publisher certificate in the current user’s Personal certificate store and `-TimestampUrl` for its HTTPS RFC 3161 timestamp service. The build signs the launcher, followed by the MSI and final setup EXE. It runs SignTool’s Authenticode trust check at each step. Run the normal install and launch check on a machine with the intended Windows policy. [Microsoft application signing guidance](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/smart-app-control)

### Publishing an in-app update

Set the version in `windows/release_identity.py`, build and qualify the Windows installer, then run `./scripts/sign-update.ps1 -Setup ./dist/Lummix.Setup.exe -Version <version>` against the final release bytes on the publisher’s Windows account. The script requires the non-exportable CNG key named `Lummix GitHub release manifests v1`. Publish `Lummix.Setup.exe`, `Lummix.Update.json` and `SHA256SUMS.txt` together on a numeric `vX.Y.Z` GitHub release. Drafts stay private. Published preview builds with numeric tags are eligible.

Keep the release in draft while testing the downloaded setup on a Windows client with Application Control enabled. Upgrade from the current public version and verify that both shortcuts open the mixer and the saved settings survive. Publishing the numeric release makes it eligible for the in-app updater and the Discord announcements.

The public key travels in the client. The private key stays in the publisher’s Windows key store. If that key becomes unavailable, distribute a manually installed build with a new verification key. The release-manifest signature authenticates in-app downloads. Windows publisher trust continues to use Authenticode.
