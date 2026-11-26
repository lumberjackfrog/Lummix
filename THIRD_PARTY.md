# Third-party software and media

Lummix is an independent Windows audio application. Product names below identify compatible software. Each component retains its own copyright, trademark and license. Keep the `licenses/` directory, the license beside the visualizer script, and the VB-Audio attribution in the Quick Start with the Windows application. The production build uses the [locked Rust manifests](windows/BUILD.md).

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
| CPython 3.12 (build and test) | The build uses CPython's Windows Installer tooling and the test suite uses the Python interpreter. [Python license](https://docs.python.org/3.12/license.html) |
| Rust runtime and crates | The Windows bundle includes the Rust Project Contributors' MIT notice in `licenses/Rust.txt`. The build collects each native dependency's upstream license files under `licenses/Rust crates/` using the locked Cargo manifests. [Rust license](https://github.com/rust-lang/rust/blob/master/LICENSE-MIT) |
| NumPy (test environment) | Audio verification uses NumPy to generate signals and compare samples. Its Windows test wheel includes the upstream NumPy and OpenBLAS notices. The installer routes buffers through the native mixer. [NumPy](https://github.com/numpy/numpy) |
| Pillow (test environment) | Image fixtures use Pillow and its wheel notices. The Windows app uses Windows Imaging Component and the locked Rust WebP decoder. [Pillow](https://github.com/python-pillow/Pillow) |
| PortAudio | The Rust audio engine loads the pinned x64 WASAPI DLL. The builder obtains it from the python-sounddevice wheel and verifies its SHA-256. The installer includes the PortAudio license. The Python binding is used by the test signal generator. [PortAudio license](https://www.portaudio.com/license.html), [binary provenance](https://github.com/spatialaudio/portaudio-binaries) |
| Python Windows test bindings | The hardware and UI verification tools use pycaw, comtypes, psutil and pywin32 in their test environment. The installed program uses Rust for Core Audio and UI Automation. [pycaw](https://github.com/AndreMiras/pycaw), [comtypes](https://github.com/enthought/comtypes), [pywin32](https://github.com/mhammond/pywin32) |
| Platform components | Keep the license and notice from the pinned Microsoft WebView2 SDK. Microsoft VC runtime files and the Universal CRT follow Microsoft's redistributable terms. The SDK headers used to compile the Lummix process-loopback bridge come from the build machine's Windows SDK. [WebView2 SDK](https://www.nuget.org/packages/Microsoft.Web.WebView2/), [Microsoft redistributables](https://learn.microsoft.com/en-us/visualstudio/releases/2022/redistribution) |
| Build and test tools | The build tooling, Playwright and its test browser have separate upstream licenses. [Playwright](https://github.com/microsoft/playwright-python) |
| Windows setup | The outer installer is built from [setup/Lummix.nsi](setup/Lummix.nsi) with NSIS. NSIS core uses the zlib/libpng license, with separate upstream terms for its compression modules. Windows Installer handles the MSI payload. [NSIS license](https://nsis.sourceforge.io/License) |

## Previous binary packages

Earlier private Windows packages used PyAV and SoundFile for a local-file decoder. The historical bundle contained FFmpeg with x264 and x265, libsndfile and several unused ASIO/other-platform libraries. Its PyAV wrapper notice alone does not cover those codec binaries or provide corresponding source. The audited clean Windows x64 build removes those components, retains the single non-ASIO x64 PortAudio DLL and includes the original basic VB-CABLE ZIP. Use the clean build for release. [FFmpeg's distribution guidance](https://ffmpeg.org/legal.html), [libsndfile licensing](https://libsndfile.github.io/libsndfile/).
