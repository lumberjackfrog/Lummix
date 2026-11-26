# Build Lummix on Windows

Use a 64-bit Windows development machine with CPython 3.12 x64 (including the `py` launcher), and NSIS 3. The MSI builder uses Python 3.12's `msilib`. Desktop playback uses the installed Evergreen WebView2 runtime. Application-specific audio capture needs Windows build 20348 or newer.

Install the x64 Rust MSVC toolchain and the C++ linker from Visual Studio Build Tools on the build machine. The installer includes the compiled sample mixer. Friends only need the Windows installer.

From the repository root, run:

```powershell
.\scripts\build.ps1
```

The script creates a repository-local virtual environment, restores pinned Python wheels, freezes the helper and builds **dist/Lummix Setup.exe**. MSI and ZIP intermediates are in `dist/`. The source tree requires no personal account credentials or installed audio driver to build. If NSIS is in a custom location, pass `-MakeNsis 'C:\path to NSIS\makensis.exe'`. Use `-MsiOnly` to build the intermediates. Supply `-Python` with the path to an existing x64 CPython 3.12 **virtual environment** to reuse its interpreter.

## Source and dependencies

- `studio/` contains the desktop interface. The list of files served and packaged is `windows/runtime/runtime_main.py:STUDIO_FILES`.
- `windows/` contains the loopback services for the media mixer and optional wrist preview. `windows/process_loopback.py` uses the Windows application-loopback API to capture the selected process tree. Audio starts on an explicit mixer action.
- `windows/runtime/` contains the private window, single-instance launcher, tray, DPI support and PyInstaller recipe. `windows/packaging/` builds the ZIP and per-user MSI. `setup/` wraps the MSI into the one-file installer.
- The compiler downloads base VB-CABLE Pack46 directly from VB-Audio and verifies its exact 1,324,680 bytes and SHA-256 `bc44e8974191a5a2427d6c979e2dab08dbb09925f3f435dfa2f0ba3e45b6eb90`. Native setup separately checks the reviewed installer hash, VB-Audio Authenticode signer and Microsoft driver catalog. Build-time verification never installs a driver. The optional setup on an end-user computer asks Windows to approve the signed vendor installer. [Original VB-CABLE package](https://vb-audio.com/Cable/) · [Distribution terms](https://vb-audio.com/Services/licensing.htm)
- The compiler downloads Microsoft.Web.WebView2 1.0.4370.26 from the official NuGet feed, verifies SHA-256 `1c0dacac84b70109b3fd42f90c53a0ba942abc3a87590d7a4e0ccd86e4a06cf4`, then checks the x64 loader's hash and Microsoft Authenticode identity. Only the loader and notices go into the package. [Microsoft SDK package](https://www.nuget.org/packages/Microsoft.Web.WebView2/1.0.4370.26) · [WebView2 runtime distribution](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution)
- `.build/` stores verified downloads, generated COM wrappers' inputs, the frozen runtime. `dist/` stores generated distributables. XSOverlay and SteamVR resources come from the user's own local installations. [Third-party notices](../THIRD_PARTY.md)

## Tests

`scripts/test-windows.ps1` builds the dependency-free Rust crate offline and compares its audio output with the NumPy renderer. The manual GitHub checks also replay generated audio against the pre-port source and measure complete 480-frame callbacks, including the Python call into Rust. Playback devices and VRChat are outside that timing.

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
