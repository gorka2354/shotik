# Shotik in Rust: behaviour contract

The Rust binary must keep everything here. The reference is the JavaScript in `src/` at tag
`electron-final`; file references below point to it. When this document and the reference disagree,
the reference wins, except in "Accepted differences".

## Process

- Arguments: `--hidden` (start in tray), `--test` (test mode), `--capture <region|full|repeat|text|record|audio|translate>`
  and `--capture=<mode>`. A second instance forwards its arguments to the first and exits; with no
  `--capture` it shows the main window. Start-menu shortcuts in `build/installer.nsh` rely on this.
- Environment (`src/main/*.js`): `SHOTIK_TEST`, `SHOTIK_GHOST`, `SHOTIK_FAKE_SCREEN`, `SHOTIK_PORT`,
  `SHOTIK_SAVE_DIR`, `SHOTIK_TRANSLATE_STUB`, `SHOTIK_REAL_CAPTURE`, `SHOTIK_FORCE_CONTROLS`,
  `SHOTIK_POPUP_REAL`, `SHOTIK_TP_TTL`, `SHOTIK_WINCAP_FAIL`. Same meaning.
- Ghost mode: no hotkeys, no selection watcher, all windows at x+20000, shown without activation.
- Test mode uses `%APPDATA%\Shotik-Test` and its own single-instance lock, so it runs next to the real app.
- App user model id `dev.shotik.app`. Autostart passes `--hidden`.

## Data: existing users keep everything

- User data directory `%APPDATA%\Shotik`: `settings.json` and `history.json`, `thumbs/`, read and
  written in the same JSON shape (`src/main/settings.js`, `src/main/history.js`). Unknown keys survive
  a load and save. Defaults and deep-merge rules are the same. A user upgrading from Electron loses nothing.
- Screenshots go to `settings.saveDir` as `{fileNamePattern}.png` with `-2`, `-3` on collision;
  recordings as `Rec_<date>_<time>.<ext>`. History holds at most 500 items, newest first. Entries whose
  file was deleted externally drop out on listing.
- Default hotkeys and every setting: `src/main/settings.js`.
- `%TEMP%\shotik-overlay-timing.log`: the last overlay's marks, overwritten, `name=ms` joined by spaces.
- `%TEMP%\shotik-bubble.log`: bubble and translate diagnostics, truncated on startup.

## MCP

Streamable HTTP, stateless, on `127.0.0.1:<settings.mcp.port>` (default 7464) at `/mcp`. Claude Code
is already connected to that address, so nothing may change for the client.

- Protocol: `src/main/mcp.js`, exactly. GET is 405, DELETE is 200, batches work, notifications get 202,
  `initialize` echoes `protocolVersion` and returns `Mcp-Session-Id`, parse errors are -32700, unknown
  tools -32602. A tool error is a result with `isError: true` and text `Error: <message>`.
- Requests whose `Host` is not a loopback name, or whose `Origin` is present and not loopback, get 403.
  Binding to 127.0.0.1 does not stop a web page in the user's browser, and a page reaching this server
  could read their screen. Clients outside a browser send no `Origin` and are unaffected.
- Tools: `registerMcpTools` in `src/main/main.js`, with identical names, descriptions, input schemas and
  reply texts: `take_screenshot`, `take_screenshot_region`, `list_windows`, `capture_window`,
  `ask_user_to_select_region`, `get_last_screenshot`, `list_screenshots`, `list_recordings`,
  `get_last_recording`, `list_displays`.
- `method` in `capture_window` replies is `printwindow`, or a window-capture fallback (see differences).
- `downscaleForAI`: images over 1600 px on the long side are resized unless `full_resolution`.
- Coordinates: global virtual-desktop DIP, the same space `list_displays` reports (`src/main/region-map.js`).

### DIP layout

Must match what Chromium computes on mixed-DPI setups. Real vector from this machine:

| Physical bounds | Scale | DIP bounds |
|---|---|---|
| 0,0 3840×2160, primary | 1.5 | 0,0 2560×1440 |
| 3840,694 1920×1080 | 1.0 | 2560,462 1920×1080 |
| -2560,419 2560×1440 | 1.0 | -2560,279 2560×1440 |

Rule: the primary sits at the origin at its own scale. Other displays attach to the display they touch:
they abut it in DIP, and the offset along the shared edge is divided by the parent's scale.

## Overlay

`src/overlay/overlay.js` is the specification: freeze frame on every display at exact bounds, hover-snap
to windows, Alt or Shift disable snapping, Shift+click selects the whole screen, drag handles, magnifier
with hex and position, `C` copies the colour, tools pen, arrow, line, rect, ellipse, highlight, blur, text,
counter with colours and sizes, undo and redo, keyboard shortcuts by physical key (`e.code`, so it works on
a Russian layout), actions copy, claude, save, pin, ocr, record, Live Text with word boxes, rubber band,
Ctrl+A, Enter, translation card. The result handling is `handleOverlayResult` in `src/main/main.js`.

Plain copy puts only the image on the clipboard; "for Claude" puts image and file path. This distinction
is deliberate, see the comment above `copyToClipboard`.

## Other windows

Main window (`src/app`), toast with hover pause and per-kind TTLs (`src/main/windows.js`), pin with wheel
zoom, drag, opacity and context menu, translate popup, selection bubble, recording border, controls with
level meters, countdown. Behaviour and timings: `src/main/windows.js`, `src/main/recorder.js`.

UI follows the system theme and accent colour, in the style of Fluent and WinUI: no decorative gradients.
Strings are in `src/main/strings.js` (en, ru); language follows `settings.language`.
Target look: `docs/rust/reference-ui/`.

## Test routes

`POST /test/<route>`, only in test mode. Reply `{ok:true,result}` or HTTP 500 `{ok:false,error}`. Keep
every route in `setupTestHandler` (`src/main/main.js`) that `test/ghost/` uses, with the same shape, plus
these, which replace running JavaScript inside windows:

| Route | Body | Result |
|---|---|---|
| `settings-set` | `{patch}` | merged settings; same side effects as the `settings:set` IPC |
| `history-list` | `{limit}` | history entries, newest first |
| `export-gif` | `{id}` | path of the GIF, or null |
| `overlay-cancel` | `{}` | cancels the open overlay session |
| `record-start` | `{displayId?,rectPhys?,rectDip?}` | starts recording; no body keeps the old default region |
| `ui-state` | `{target,index?}` | JSON snapshot of that window's `State` |
| `ui-action` | `{target,index?,action}` | dispatches an `Action`, returns the new snapshot |
| `capture-page` | `{target,index?,path}` | renders the window to PNG in-process; `{path,size}`; works off-screen |

Targets: `overlay`, `main`, `toast`, `pin`, `tp`, `bubble`, `rec-controls`, `rec-border`. Exact state and
action shapes are in `docs/rust/TEST-ROUTES.md`. `exec-js` does not exist in Rust.

`overlay-timing` returns the marks as an array of `name=ms` strings. Rust must provide at least `grab`,
`winCreate`, `painted`, `show`, measured from the same point: the start of the overlay session.

## Accepted differences

- **Container:** video is H.264 and AAC in `.mp4` by default instead of VP9 in `.webm`. Audio stays
  AAC in `.m4a`. `recording.format` values stay valid in `settings.json`.
- **Fallback name:** `capture_window`'s fallback reports its own method name instead of `desktopCapturer`.
- **Removed:** `exec-js`, `win-sources` (Chromium diagnostics), Microsoft Store packaging for now.
- **macOS:** not in the first Rust release.
- **Installer:** not electron-builder, but the same install location, shortcuts and upgrade over Electron.
- **Secure desktop:** a capture fails with an explicit error instead of returning a black frame.
- **Clipboard:** an RLE-compressed image on the clipboard is refused with an error, because it is untrusted input.
- **Display ids:** computed differently, so a `lastRegion` remembered by Electron falls back to the primary
  display once. Chromium's id did not survive a reboot anyway.
- **Rendering:** no drawn soft shadows; windows use DWM rounded corners with the native shadow.

Anything else that changes user-visible behaviour is a bug unless this list is updated by the lead.
