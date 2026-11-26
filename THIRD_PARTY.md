# Third-party software and media

Lummix is an independent Windows audio application. Product names below identify compatible software. Each component retains its own copyright, trademark and license. For a Windows distribution, keep the `licenses/` directory, the license beside the visualizer script, and the VB-Audio attribution in the Quick Start with the application. The pinned build dependencies are listed in [windows/requirements-build.txt](windows/requirements-build.txt).

## What comes from the user's PC

| Product | How Lummix uses it |
| --- | --- |
| Spotify | The user installs Spotify separately. Lummix reads the Windows media session and its thumbnail, uses supported Windows transport and accessibility interfaces, and uses Windows process-loopback audio when the user starts the mix. It does not require a Spotify Web API key. The application installer contains no Spotify client, catalog, track or album cover. |
| VRChat | The user selects the Windows recording endpoint as the microphone in their installed VRChat client. VRChat software, worlds, player data and SDK assets are separate. |
| XSOverlay by Xiexe | The optional desktop wrist preview reads assets from the user's own XSOverlay installation. When the user installs the add-on, Lummix registers its own local panel and icon and adds a marked block to the installed wrist page. Its add-on and local bridge provide the controls. The upstream wrist page, fonts, icons, textures and XSOverlay application belong with XSOverlay and stay outside the Lummix distribution. |
| Microsoft Edge WebView2 Runtime | The desktop window uses the system's Evergreen Runtime. The Windows bundle includes the SDK's x64 WebView2Loader and corresponding SDK license and notice. |

Music and artwork remain the rights holders' content. An installed player or working Windows audio route does not grant a music rebroadcast or public-performance license. Spotify grants personal, non-commercial access and restricts redistribution. Use descriptive product names and independent Lummix artwork in repository images and marketing. [Spotify terms](https://www.spotify.com/legal/end-user-agreement/), [Spotify brand guidelines](https://developer.spotify.com/documentation/design), [XSOverlay official listing](https://store.steampowered.com/app/1173510/XSOverlay/), [VRChat terms](https://hello.vrchat.com/legal), [WebView2 deployment](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution).

## Basic VB-CABLE

**VB-CABLE is donationware by VB-Audio. Contributions are welcome.** Lummix uses the original Windows base driver. The recording endpoint can have the friendly name **Lummix** in Windows. Its driver and publisher remain VB-Audio. [Product and donations](https://vb-audio.com/Cable/), [licensing and distribution conditions](https://vb-audio.com/Services/licensing.htm).

The distribution permission covers the unmodified basic package included with an application, including installer deployment, while end users can identify VB-Audio and support it. VB-Audio specifies additional licensing for organizations whose users cannot independently identify or contribute to the product. Its separately licensed A+B and C+D packages are outside this permission. The pinned original `VBCABLE_Driver_Pack46.zip` is 1,324,680 bytes, SHA-256 `bc44e8974191a5a2427d6c979e2dab08dbb09925f3f435dfa2f0ba3e45b6eb90`. [Vendor download](https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip).

## Bundled and build-time components

| Component | License and notices to retain |
| --- | --- |
| simplex-noise.js 4.0.3, Jonas Wagner and credited contributors | MIT. Retain the header of the bundled browser script and its [license](studio/visualizer-simplex-noise-LICENSE.txt). [Upstream](https://github.com/jwagner/simplex-noise.js) |
| CPython 3.12.10 | PSF license and incorporated software licenses. Use the complete Windows distribution's `LICENSE.txt` in the release, including notices for its bundled libraries. [Python license](https://docs.python.org/3.12/license.html) |
| Rust runtime and crates | The Windows bundle includes the Rust Project Contributors' MIT notice in `licenses/Rust.txt`. The build collects each native dependency's upstream license files under `licenses/Rust crates/` using the locked Cargo manifests. [Rust license](https://github.com/rust-lang/rust/blob/master/LICENSE-MIT) |
| NumPy (test environment) | Audio verification uses NumPy to generate signals and compare samples. Its Windows test wheel includes the upstream NumPy and OpenBLAS notices. The installer routes buffers through the native mixer. [NumPy](https://github.com/numpy/numpy) |
| Pillow (test environment) | Image fixtures use Pillow and its wheel notices. The Windows app uses Windows Imaging Component and the locked Rust WebP decoder. [Pillow](https://github.com/python-pillow/Pillow) |
| python-sounddevice and PortAudio | MIT for python-sounddevice and PortAudio. Include the PortAudio copyright and permission notice with the x64 WASAPI-capable DLL. The ordinary Windows route uses the non-ASIO library. [python-sounddevice](https://github.com/spatialaudio/python-sounddevice), [PortAudio license](https://www.portaudio.com/license.html), [binary provenance](https://github.com/spatialaudio/portaudio-binaries) |
| Python Windows bindings | pycaw and comtypes are MIT. pywin32 has its own PSF-style notice. Keep each upstream license. [pycaw](https://github.com/AndreMiras/pycaw), [comtypes](https://github.com/enthought/comtypes), [pywin32](https://github.com/mhammond/pywin32) |
| Local HTTP and utility dependencies | aiohttp uses Apache-2.0 and MIT. aiosignal, frozenlist, multidict, propcache and yarl use Apache-2.0. aiohappyeyeballs and typing_extensions use PSF-2.0. attrs, charset-normalizer and cffi use MIT. idna, psutil and pycparser use BSD-3-Clause. packaging offers Apache-2.0 or BSD-2-Clause. Preserve each component's notices. `licenses/COMPONENTS.txt` identifies versions collected during the build. [aiohttp](https://github.com/aio-libs/aiohttp), [charset-normalizer](https://github.com/jawah/charset_normalizer) |
| Platform components | Keep the license and notice from the pinned Microsoft WebView2 SDK. Microsoft VC runtime files and the Universal CRT follow Microsoft's redistributable terms. The SDK headers used to compile the Lummix process-loopback bridge come from the build machine's Windows SDK. [WebView2 SDK](https://www.nuget.org/packages/Microsoft.Web.WebView2/), [Microsoft redistributables](https://learn.microsoft.com/en-us/visualstudio/releases/2022/redistribution) |
| Build and test tools | PyInstaller's bootloader exception permits packaging applications subject to dependency terms. The freezer, build tooling, Playwright and its test browser have separate upstream licenses. A browser used for testing need not ship with the app. [PyInstaller license and exception](https://pyinstaller.org/en/stable/license.html), [Playwright](https://github.com/microsoft/playwright-python) |
| Windows setup | The outer installer is built from [setup/Lummix.nsi](setup/Lummix.nsi) with NSIS. NSIS core uses the zlib/libpng license, with separate upstream terms for its compression modules. Windows Installer handles the MSI payload. [NSIS license](https://nsis.sourceforge.io/License) |

## Previous binary packages

Earlier private Windows packages used PyAV and SoundFile for a local-file decoder. The historical bundle contained FFmpeg with x264 and x265, libsndfile and several unused ASIO/other-platform libraries. Its PyAV wrapper notice alone does not cover those codec binaries or provide corresponding source. The audited clean Windows x64 build removes those components, retains the single non-ASIO x64 PortAudio DLL and includes the original basic VB-CABLE ZIP. Use the clean build for release. [FFmpeg's distribution guidance](https://ffmpeg.org/legal.html), [libsndfile licensing](https://libsndfile.github.io/libsndfile/).
