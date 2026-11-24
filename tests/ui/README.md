# Studio smoke tests

Run these commands from the repository root with Python 3.10 or later.

```sh
python -m pip install -r tests/ui/requirements.txt
python -m playwright install chromium
python -m unittest discover -s tests/ui -v
```

The suite loads the shipping HTML, CSS and JavaScript in isolated Chromium profiles at desktop and phone sizes. All HTTP requests are intercepted, and the optional XSOverlay WebSocket is disabled. It checks the single Settings entry, full-width setup action, microphone selection, gain snapping, and the start/stop request contract using synthetic devices. No Windows helper, microphone, Spotify installation or VB-CABLE driver is required. For an existing compatible Chromium executable, set the optional `LUMMIX_TEST_CHROMIUM` environment variable to its absolute path.

A WebView2 bridge fixture checks the desktop title bar at 1450, 390 and 320 pixels, including keyboard actions, maximize/fullscreen state changes, dragging and double-clicking. The browser and wrist views run without desktop window buttons. Real Windows resizing, taskbar and tray behavior are tested separately with the native host.
