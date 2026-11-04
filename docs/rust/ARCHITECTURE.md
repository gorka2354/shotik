# Shotik in Rust: architecture

Goal: replace the Electron app with a native Rust binary while keeping the behaviour in
`CONTRACT.md`. Success is measured by `bench/run.js`, the same script against both versions.
The first Rust release is Windows-only. macOS stays on the last Electron build until it has its
own implementation of the platform ports; nothing in `shotik-core` may assume Windows.

## Layout

```
crates/
  shotik-core/      Domain types, port traits, pure logic. No OS calls, no UI, no async runtime.
  shotik-win/       Windows implementation of the platform ports (uses the `windows` crate).
  shotik-fake/      Fake implementations of the ports for tests and ghost mode.
  shotik-mcp/       HTTP server: MCP JSON-RPC at /mcp, test routes at /test/*, MCP tool definitions.
  shotik-recorder/  Video and audio capture, encoding, muxing; GIF export.
  shotik-ui/        All windows. Each window = pure state + view (see below).
  shotik-app/       Orchestration: triggers, overlay result handling, selection logic, tray menu.
  shotik/           Binary. Composition root only: picks real or fake ports, starts everything.
spikes/             Throwaway prototypes, excluded from the workspace.
```

Dependencies point inward: everything depends on `shotik-core`; `shotik-core` depends on nothing of
ours. `shotik-app` is the only crate that knows all the others, except the binary that wires them.

## Ports

Everything external sits behind a trait in `shotik-core/src/ports.rs`: displays, screen capture,
window enumeration and capture, clipboard, OCR, translation transport, shell, dialogs, hotkeys, tray,
theme, autostart, selection watching, audio devices, the UI itself. Replacing an implementation must
not touch the core. Ports are synchronous, `Send + Sync`, and return typed errors. Callers run slow
ones on a blocking thread. A port that needs to report later takes a callback, so the core does not
depend on an async runtime.

Port traits stay small. If a window or tool needs something new, add one method, not a parallel path.

## Threads

- **Main thread:** the UI event loop.
- **Tray and hotkeys:** each owns a dedicated thread with its own message-only window and message loop,
  so they need nothing from the UI toolkit and work while no window exists.
- **Tokio runtime (few workers):** MCP and test HTTP server, orchestration, spawn_blocking for slow ports.
- **Recorder threads:** capture and encode run on dedicated threads, never on the UI thread.

Other threads talk to the UI only through `UiPort`, which posts to the event loop.

## UI: pure state plus view

Every window is split in two so that logic is testable without a window and the UI toolkit stays
replaceable:

```
crates/shotik-ui/src/<window>/state.rs   State, Action, reduce(&mut State, Action) -> Effects. No toolkit types.
crates/shotik-ui/src/<window>/view.rs    Toolkit code: draws State, turns input into Actions.
```

Hit-testing, selection geometry, annotation history, Live Text word selection and timers live in
`state.rs` with unit tests. `view.rs` holds as little logic as it can. Test routes `ui-state` and
`ui-action` read `State` and dispatch `Action` as JSON, which is how end-to-end tests drive windows.
`capture-page` renders a window to PNG in-process and must work off-screen. See `CONTRACT.md`.

## Resident footprint

Shotik sits in the tray almost all the time. In that state it must hold no window surfaces, no GPU
device if avoidable and no decoded images. Windows are created on demand or kept as cheap hidden
stubs; frames and overlays are freed when a session ends. Idle memory is a headline metric.

## Latency

Hotkey to visible overlay is the product's main feel. Capture the display under the cursor first and
show it immediately; other displays follow in the background. Do not encode PNG on that path. Timing
marks (`grab`, `winCreate`, `painted`, `show`) are recorded the same way as in Electron, see `CONTRACT.md`.

## Video

Windows Graphics Capture for frames, Media Foundation for hardware H.264 and AAC into MP4, WASAPI for
system loopback and microphone. This changes the default container from WebM to MP4, which is an
accepted difference (`CONTRACT.md`). Chromium's MediaRecorder needed the WebM and MP4 index patching
in `src/main/webm-index.js`, `mp4-index.js` and `media-index.js`: a correctly finalized MP4 does not,
so they are not ported, unless a crash-safe fragmented file turns out to need it.

## Testing layers

1. Unit tests in each crate (`cargo test`), including everything ported from `test/unit/`.
2. Contract tests in `test/ghost/`, Node.js over HTTP against a ghost instance, the same files for
   Electron and Rust (`SHOTIK_IMPL=electron|rust`, see `test/ghost/adapter.js`).
3. `bench/run.js` for numbers, on the same scenario as the Electron baseline in `bench/results/`.

Ghost mode is not full coverage: it skips real focus, real hotkeys and on-screen painting. Anything
that depends on those needs a separate real-mode check with the user's agreement.
