# Studio smoke tests

Run these commands from the repository root with Python 3.10 or later and a current stable Rust toolchain.

```sh
rustup target add wasm32-unknown-unknown
cargo test --locked --manifest-path windows/controls-rs/Cargo.toml --target-dir .build/controls
python windows/controls-rs/build_controls.py
python -m pip install -r tests/ui/requirements.txt
python -m playwright install chromium
python -m unittest discover -s tests/ui -v
```

The suite loads the shipping HTML, CSS, JavaScript and Rust WebAssembly in isolated Chromium profiles at desktop and phone sizes. All HTTP requests are intercepted, and the optional XSOverlay WebSocket is disabled. It checks the single Settings entry, full-width setup action, microphone selection, gain snapping, and the start/stop request contract using synthetic devices. No Windows helper, microphone, Spotify installation or VB-CABLE driver is required. For an existing compatible Chromium executable, set the optional `LUMMIX_TEST_CHROMIUM` environment variable to its absolute path.

A WebView2 bridge fixture checks the desktop title bar at 1450, 390 and 320 pixels, including keyboard actions, maximize/fullscreen state changes, dragging and double-clicking. The browser and wrist views run without desktop window buttons. Real Windows resizing, taskbar and tray behavior are tested separately with the native host.

On Windows, set `LUMMIX_HOST_DLL` to the output of a release build of `windows/host-rs` to run the wrist suite. It serves the production panel from a private native loopback listener and compiles the same WebAssembly module under its content security policy. A private synthetic mixer verifies the accessible mute buttons and keyboard fader with the saved snap preference. The wrist suite also serves a temporary vendor tree through the native preview, verifying file policy, CSS rendering, and the setup token contract with a simulated driver controller. Both tests exercise private loopback listeners and leave the installed app and devices untouched.
