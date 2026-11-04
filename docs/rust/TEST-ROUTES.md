# Ghost UI test protocol

`test/ghost/adapter.js` is the boundary between the Node tests and the binary. `SHOTIK_BIN` points
to the executable; by default it is `release/shotik.exe` under `CARGO_TARGET_DIR` or `target`. It starts
with `--hidden --test`, `SHOTIK_TEST=1`, `SHOTIK_GHOST=1`, the fake-screen PNG and translation stub. Never send real input, register hotkeys, activate windows or capture a real
display/audio device in these tests.

The harness creates a fresh private `%APPDATA%/Shotik-Test`, `%TEMP%`, save
directory and history for **each** app. Rust test mode must respect the child
process's `APPDATA`, `TEMP` and `SHOTIK_SAVE_DIR`. The Electron-only bootstrap
sets the equivalent app paths before loading the unchanged reference app. It
also makes ghost windows nonfocusable before their initial show and disables
off-screen background throttling so recording controls can be captured. No
existing settings/history are copied. An STA clipboard helper eagerly saves all
original formats in memory and restores them at teardown. Occupied ports are
errors; the harness never attaches to or quits the occupant.
Adapter readiness waits for both the owned HTTP listener and `main.ready`.
A cold private profile must finish UI initialization before tests start short
popup/toast timers or record a short clip; HTTP `ping` alone is not that barrier.

## Envelope and coordinates

Test-only HTTP: `POST http://127.0.0.1:$SHOTIK_PORT/test/<route>` with JSON.
Success is HTTP 200 `{ "ok": true, "result": <value> }`. Errors are HTTP 500
`{ "ok": false, "error": "message" }`; do not return an empty successful state
for an unknown target/action. Objects below are the unwrapped `result`.

`ui-state`: `{ "target": "main", "index": 0 }`.
`ui-action`: `{ "target": "main", "index": 0, "action": { "type": "navigate", "page": "text" } }`.
`index` defaults to zero, selecting a pin/overlay in creation order. An action
returns the target's new state in the shapes below. Asynchronous work can still
be loading when the action returns; tests poll state. `reload` completes after
the main UI has loaded persisted settings and its language dictionary.

Window/screen coordinates are **DIP** unless explicitly called image pixels.
Global points include the ghost x offset of 20000. Overlay actions use local
viewport DIP (without this offset); `selection` uses physical image pixels.
`capture-page` accepts `{target,index?,path}` and returns
`{ "path": "absolute/file.png", "size": { "width": 1120, "height": 740 } }`.
It renders in-process off-screen, showing an unshown surface without activation
if necessary. Its PNG pixel size can differ from the DIP viewport at mixed DPI.
It supports `main`, `overlay`, `toast`, `pin`, `tp`, `bubble`, `rec-controls`,
`rec-border`. The reference generator uses all of these targets.

## `main` state

```json
{
  "ready": true,
  "page": "text",
  "title": "Shotik",
  "text": {
    "input": "hello world",
    "result": "[stub] hello world",
    "resultVisible": true,
    "loading": false
  },
  "microphone": {
    "enabled": true,
    "savedEnabled": true,
    "deviceRowVisible": true,
    "deviceId": "unplugged-device",
    "deviceLabel": "Selected microphone is unavailable"
  }
}
```

| Field | Meaning / consumer |
|---|---|
| `ready: boolean` | Settings and localized microphone label are loaded; `recorder.test.js`, reference generator. False while booting/reloading. |
| `page: "history"\|"text"\|"claude"\|"settings"\|null` | Active navigation page; reference generator checks navigation before capture. `null` only before initialization. |
| `title: string` | Actual native window title (Electron `document.title`); MCP window enumeration test sets a unique title. |
| `text.input: string` | Text entered in the manual translation input; reference state. |
| `text.result: string` | Currently displayed result, including localized loading/error text; `texttab.test.js` checks the stub text. Do not return a cached successful translation while loading. |
| `text.resultVisible: boolean` | Result panel is unhidden; `texttab.test.js` explicitly asserts it. Independent of which navigation page is active. |
| `text.loading: boolean` | The result currently has its loading presentation; reference generator waits for completion. |
| `microphone.enabled: boolean` | Displayed screen-recording microphone checkbox; asserted after reload in `recorder.test.js`. |
| `microphone.savedEnabled: boolean` | Backing setting `recording.microphone`; test waits for the real settings update, not merely a checkbox animation. |
| `microphone.deviceRowVisible: boolean` | Device selector row is unhidden; shown when the screen or audio recorder uses the mic. Same visibility semantics as the original test. |
| `microphone.deviceId: string` | Selected device option, including a persisted missing device id. |
| `microphone.deviceLabel: string` | Localized label of that selected option; the missing-device English label is asserted verbatim. |

## `main` actions

| `action` JSON | Meaning / consumer |
|---|---|
| `{"type":"navigate","page":"history"}` | Activate one of the four pages above; reference generator. |
| `{"type":"quick-translate","text":"hello world"}` | Navigate to Text, enter text and activate its Translate control through the normal UI handler; `texttab.test.js`, reference generator. Does not copy the result. |
| `{"type":"set-title","title":"ShotikGhostTarget"}` | Set the actual window title, so native enumeration/capture can find this test window; `mcp-tools.test.js`. |
| `{"type":"reload"}` | Recreate/reload main view state from persisted settings, including microphone options and localization; `recorder.test.js`. Does not reuse unsaved widget values. |
| `{"type":"microphone-toggle"}` | Activate the screen-recording microphone checkbox and its normal persistence handler; `recorder.test.js`. |
| `{"type":"microphone-select","deviceId":"unplugged-device"}` | Apply this device id through the same settings/render path as the UI, retaining an unavailable option; `recorder.test.js`. |
| `{"type":"hotkey-focus","field":"region"}` | Navigate to Settings and focus the laid-out, enabled region hotkey editor **inside** the view; do not activate/focus the native window. Electron verifies local DOM focus, then dispatches the local focus event for the nonfocusable ghost view. The real editor handler suspends hotkeys; `mcp-tools.test.js` asserts the global suspension state. |
| `{"type":"hotkey-blur","field":"region"}` | Blur that editor through the normal handler, resuming shortcuts; `mcp-tools.test.js`. Only `region` is used. |
| `{"type":"history-clear"}` | Clear the history index with `deleteFiles:false`, refresh its view; `mcp-recordings.test.js`. |
| `{"type":"history-pin","id":"fixture-1"}` | Pin an existing history image through the normal pin operation; reference generator. |

## `tp` state

```json
{ "original": "hello", "translated": "[stub] hello", "loading": false }
```

`original` and `translated` are the exact currently displayed strings. During
loading `translated` is the loading message and `loading` is true; after a result
it is the result text and `loading` is false. `bubble.test.js` checks preservation
of selection text and its stub translation (including the clear/click race);
`ocr.test.js` checks that recognized text reached this popup. The reference
generator captures both loading and result states. No `tp` ui-action is needed:
the existing `show-tp-loading`, `resolve-tp`, bubble and hover routes are shared.

## `bubble` action result

```json
{ "bubble": true, "popup": false }
```

Both booleans are actual native visibility after a simulated watcher press.
The adapter uses `ui-action` with
`{"type":"pointer-down","point":{"x":20619,"y":354}}` on target `bubble`.
`point` is **global DIP**, including the ghost offset. Dispatch the same global
click-away logic for both bubble and translate popup; do not click the desktop
and do not activate the bubble. `bubble.test.js` verifies that an outside press
hides the bubble/popup, while an inside press leaves the bubble visible.

Electron converts DIP to physical pixels in a windowless helper before calling
`sim-down`, because `onMouseDown` converts physical to DIP. At x+20000 the native
conversions can choose different nearest monitors: the helper verifies the
round trip and corrects its physical point against `screenToDipPoint`. A single
`devicePixelRatio` multiplier (or unchecked `dipToScreenPoint`) is insufficient
on the reference three-monitor 150% primary layout. Rust's semantic action
starts in DIP and must test the same bounds/hide logic.

## `rec-controls` state

```json
{
  "meters": [
    { "input": "system", "track": 22, "fill": 18.26 },
    { "input": "microphone", "track": 22, "fill": 18.26 }
  ],
  "diagnostics": {
    "audioCodec": "aac",
    "indexed": true,
    "screenVideoTracks": 0,
    "screenAudioTracks": 0
  }
}
```

`meters` is the rendered list, system first then microphone, including only
enabled inputs. `track` and `fill` are actual laid-out widths in DIP, **not** a
style percentage or a raw audio peak. Disable off-screen meter transitions for
this measurement. `audio-recording.test.js` requires both fills to exceed 20%
of their track widths and checks the order. An absent control surface has an
empty meter list; diagnostics must remain readable even after recording stops,
or when ghost mode omits controls.

`diagnostics` describes the current/latest recording, never fabricated defaults:

| Field | Meaning / consumer |
|---|---|
| `audioCodec: "aac"\|null` | AAC audio-only encoder was actually started; `audio-recording.test.js`. Null before startup or for another codec. Electron derives this from the exact AAC start trace previously asserted. |
| `indexed: boolean` | Final duration/seek index successfully written. Electron's `index ok` becomes true; Rust becomes true only after successful container finalization. `audio-recording.test.js`, `recording-seek.test.js`. False before finalization or on failure. |
| `screenVideoTracks: number\|null` | Video tracks obtained from the screen source (not output tracks); microphone-only audio requires zero, proving no screen acquisition. Null before the source is known. |
| `screenAudioTracks: number\|null` | Audio tracks obtained from that same screen source, excluding the separate microphone; microphone-only audio requires zero. Both track counts are checked in `audio-recording.test.js`. |

There are no `rec-controls` ui-actions in the adapter. The existing record and
status routes remain shared; `SHOTIK_FORCE_CONTROLS=1` creates ghost controls
and, for screen recording, the border for rendered-meter tests/references.

## `overlay` state and reference actions

`docs/rust/reference-ui/` shows these states as Electron drew them, from fake data.

```json
{
  "ready": true,
  "mode": "selected",
  "selection": { "x": 360, "y": 210, "w": 1650, "h": 960 },
  "viewport": { "width": 2560, "height": 1440 },
  "image": { "width": 3840, "height": 2160 },
  "toolbarVisible": true,
  "hintsVisible": false,
  "textMode": false,
  "wordCount": 0,
  "annotationCount": 0
}
```

`ready` means the fake frame is decoded/painted. `mode` is `idle`, `selecting`
or `selected`. `selection` is null before selection, otherwise its image-pixel
rectangle. `viewport` is local DIP extent; `image` is the physical source size.
`toolbarVisible` and `hintsVisible` describe the actual unhidden controls.
`textMode` indicates Live Text; `wordCount` counts recognized word boxes;
`annotationCount` counts committed annotations.

| `action` JSON | Effect |
|---|---|
| `{"type":"select","rect":{"x":240,"y":140,"w":1100,"h":640}}` | Select this local-DIP rectangle, enter `selected` mode and lay out the normal toolbar. Convert to image pixels in state. |
| `{"type":"annotate","annotations":[...]}` | Replace committed annotations, repaint with the normal renderer and update Undo. Geometry below is local DIP; convert to image pixels internally. |
| `{"type":"live-text"}` | Enter Live Text through the normal OCR handler on the selected **fake** image crop. Real OCR, no fake result injection. |
| `{"type":"text-select-all"}` | Activate Live Text's Select All control. |
| `{"type":"copy-color","hex":"#3E9EFF"}` | Normal color-copy IPC/effect, including the color toast. Clipboard restoration is owned by the harness. |

The reference annotation array uses exactly these variants (colors are CSS hex;
stroke/font sizes are DIP, as in the real toolbar). Coordinates and sizes are
finite numbers; `n` is a positive integer and `lines` is an array of strings.
The listed fields are required for their respective variant:

```json
[
  {"type":"rect","x1":320,"y1":220,"x2":740,"y2":360,"color":"#FF4D5E","stroke":4},
  {"type":"arrow","x1":900,"y1":420,"x2":750,"y2":300,"color":"#3E9EFF","stroke":4},
  {"type":"counter","x":300,"y":210,"n":1,"color":"#FF9F2E"},
  {"type":"text","x":340,"y":390,"lines":["Review this area"],"fontPx":23,"color":"#FFFFFF"}
]
```

No other annotation variant/action is required by this adapter. `select` and
`annotate` set semantic test state; rendering is the product renderer, not a
replacement drawn by the reference generator.

## `pin` state

`{"ready":true}` means the pinned image has finished decoding with nonzero
natural dimensions. The reference generator waits for this before capture.
There are no pin ui-actions; `main/history-pin` creates the pin.

## Other adapter operations

| Operation / Rust route | Body and result |
|---|---|
| Settings / `settings-set` | `{patch}` -> merged settings, with the same side effects as settings IPC. Tests change only fake-profile settings. |
| History / `history-list` | `{limit}` -> newest-first history entries; default 100. Same entries as the reference history IPC, including `kind`, `file`. |
| GIF / `export-gif` | `{id}` -> GIF absolute path or null. Corresponds to history GIF export. Available for contract callers; current eight test files do not invoke it. |
| Cancel / `overlay-cancel` | `{}` -> cancel the current overlay session. The generator waits for `state.overlayOpen === false`. |
| Record / `record-start` | `{displayId?,rectPhys?,rectDip?}` -> existing recorder start result. Rectangles have `x,y,w,h`, physical source pixels and local DIP respectively. No body preserves the reference default 320×180 DIP region at (20,20) on primary. The current Electron route supports only this default. |

All other test routes used by the suite (`state`, selection/translate routes,
OCR routes, `trigger`, `record-status`, `record-stop`, toast/hotkey routes,
`overlay-timing`, `sim-action`, `set-theme`, `pins`, `quit`, etc.) keep the
reference `CONTRACT.md` behavior. `show-main` must respect ghost mode; Electron's
adapter instead uses an off-screen capture to avoid the legacy route's focus.

Accepted implementation differences are limited to `CONTRACT.md`: Rust's
default video extension is `.mp4`; Electron's is `.webm`. A forced window-capture
fallback must report a non-PrintWindow method (Electron specifically reports
`desktopCapturer`). These change expected representation, not assertion strength.

## Media checks without a renderer

`media.js` finds FFmpeg through `FFMPEG_PATH` (an executable or its directory),
then `PATH`, then `%LOCALAPPDATA%/Microsoft/WinGet/Packages/Gyan.FFmpeg*/**/bin`.
`ffprobe` is resolved beside configured FFmpeg, otherwise by the same search.
Missing tools produce an explicit installation/configuration error.

Decode the first audio channel with FFmpeg to little-endian float32 PCM at its
native sample rate. For `i` in `[floor(rate*0.2), min(length,start+rate))`, compute
`2*hypot(sum(sample[i]*cos(2*pi*f*i/rate)),sum(sample[i]*sin(2*pi*f*i/rate)))/(end-start)`.
Frequencies remain 440 Hz system and 880 Hz mic; enabled amplitudes must exceed
0.035, disabled amplitudes must be below 0.015, exactly as before. Duration is
decoded sample count / sample rate.

Electron still probes its muted player through its existing route. Rust uses
ffprobe's finite container duration, then the same muted, windowless Media
Foundation seek check. `probeMedia` returns `{duration,seekTo,seeked,position}`;
`seeked` is true only when MF can seek and the actual position is within 0.2 s.
The original tests also independently check MF's duration (0.1 s tolerance) and
a later seek (0.3 s tolerance). No synthetic successful media result is returned.
