# Troubleshooting (Windows)

## The app does not start

A failure before any window exists is written to `%TEMP%\shotik-fatal.log`. If Shotik is already running,
a second launch only forwards its arguments to the first instance and exits: look for the tray icon.

## Claude cannot reach the MCP server

Shotik listens on `127.0.0.1:7464`. If another program holds the port, Shotik says so in a toast
("MCP server failed to start") and on the Claude page. Requests with a foreign `Host` or `Origin` get
HTTP 403: this is what keeps web pages from reaching the server, so connect from a local client.

## Recording fails right away

- **Windows N or KN editions** have no Media Foundation until the
  [Media Feature Pack](https://support.microsoft.com/help/4516397) is installed. Shotik reports this as
  an error instead of writing an empty file.
- **A microphone selected in an older version** may be reported as unavailable: Electron stored its own
  device ids. Pick the device again in Settings → Screen recording. The system-default choice carries over.

## After an upgrade from an Electron version

Settings, history and screenshots stay where they were, in `%APPDATA%\Shotik`. Fields that a new version
cannot read are reset one by one, so one damaged value costs that value only. The last Electron build,
including the macOS DMG, is [v1.3.1](https://github.com/gorka2354/shotik/releases/tag/v1.3.1).

## An update does not arrive or does not install

Settings show the version and what the last check found. A check that failed says why, and is tried again within the hour; a
portable copy opens the release page, because an installer would put a second copy elsewhere. The button waits while a
recording runs or the capture overlay is open. If an antivirus blocks the installer, Shotik says so and keeps running; you can
always install the latest version by hand from the [releases page](https://github.com/gorka2354/shotik/releases/latest), and
your settings and history stay. Details: `%TEMP%\shotik-bubble.log`.

If Shotik does not come back after an update, start it from the Start menu.

## Uninstalling

Uninstalling removes the program, its shortcuts and its autostart entry. Your settings, history and
screenshots stay on disk: delete `%APPDATA%\Shotik` and your screenshots folder yourself if you want them gone.
