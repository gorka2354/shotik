# Live check

`test/ghost/` runs off-screen and cannot see focus, DPI changes, native menus and dialogs, or a real audio
device. This does. It found bugs that every other test had passed, so run it before a release.

**It takes over the desktop.** It shows real windows, moves the mouse, clicks and types, and plays a very quiet
1 kHz tone for a few seconds. Run it only on a machine nobody is using, and only with its owner's agreement
(`AGENTS.md`, rule 1).

It protects the user as far as it can: it refuses to start unless the machine has been idle (120 s by default),
it stops at the first input it did not send, it sends a click or a key only after checking that the window
under it belongs to the app under test or to its own test window, and it saves and restores the clipboard.
Screenshots of the real screen stay in a temporary folder whose path it prints: delete it when you are done.

```bash
cargo build --release -p shotik
node test/live/run.js target/release/shotik.exe            # everything, with test routes, private profile
node test/live/run.js target/release/shotik.exe --only tools,snap
node test/live/selection.js target/release/shotik.exe      # production mode: selection watcher, bubble, translate
node test/live/installed.js                                # the installed app: PrintScreen, the real tray icon
```

`run.js` uses its own hotkeys (`Ctrl+Alt+Shift+F5` to `F11`), so it runs next to an installed Shotik. The
selection watcher does not start in test mode, which is why `selection.js` is separate. It runs without test
routes, on a private `%APPDATA%`, with the MCP server on port 7699, and it needs no other Shotik running: a second production instance would only
forward its arguments and exit. `microphone_signal` needs VB-Audio Cable; it
sends a tone through it, which nobody hears. `microphone` records two seconds from the real default microphone,
measures the level and deletes the file. Mixed-DPI checks need monitors of different scale.

`installed.js` runs on the user's real profile, so it only opens windows and closes them again: nothing is saved,
copied or translated. Windows hides a new tray icon in a flyout, and `Shell_NotifyIconGetRect` then answers with
the arrow's rectangle, so it opens the flyout and finds the icon through UI Automation.

`russian_layout` switches the keyboard layout, which is global on current Windows, and switches it back.

Not covered: the UAC secure desktop, exclusive fullscreen, HDR, Windows 10.
