# Overlay latency research

How fast can the selection overlay appear after the hotkey? Measured on a
2560×1440 + 1920×1080 Windows setup (Electron 33).

## Where the time went

| Step | Cost |
|---|---|
| First `desktopCapturer.getSources` in a process (cold engine start) | ~300–600 ms |
| `getSources` **per display** (each call captures *all* screens) | ~300 ms × N |
| `getSources` **once**, warm | **~25 ms** |
| `nativeImage.toPNG()` (old freeze-frame handoff) | ~120 ms / display |
| `nativeImage.toBitmap()` (raw BGRA) | ~2 ms |
| Half-resolution thumbnail | no faster — capture of all screens dominates |

Key facts (confirmed empirically and in the Electron docs):

1. **`getSources` always captures every screen**, even when you ask for one
   display's thumbnail. So calling it once per monitor multiplies the cost.
   *("even though we only need a screenshot of one display, we can only fetch
   thumbnails for all of them" — Electron docs / yal.cc.)*
2. **The first call is a cold start** — it spins up the capture engine (~300–600 ms).
3. **PNG encode + disk round-trip** was a large, avoidable cost.

## What we shipped

- **0.5.6** — pre-warm the capture engine at startup (kept hot on a 30 s timer)
  and hand the freeze frame to the overlay as a raw BGRA bitmap over IPC instead
  of a PNG file. ~800 ms → ~270 ms.
- **0.6.0** — **cursor-display-first**: capture the monitor under the cursor with
  a single `getSources` and show it immediately (~25–50 ms warm); capture the
  other monitors natively in the background. The monitor you're actually on
  appears near-instantly, with no resolution/quality trade-off. A single
  `getSources` can't return each monitor at its native resolution (thumbnailSize
  is global and upscales smaller displays), so non-cursor monitors get their own
  native capture a moment later rather than a blurry immediate one.

## Considered / future

- **Persistent `getDisplayMedia` stream** (the Electron-recommended path for
  frequent capture): keep a live screen stream warm and grab the current frame
  instantly (<16 ms) on hotkey. Downside: a continuously-running capture uses
  GPU/CPU/battery even when idle. Would need a start/stop policy (e.g. only while
  the app is focused, or opt-in "instant mode").
- **Native capture addon** (Windows Graphics Capture / DXGI Desktop Duplication):
  sub-frame capture, but requires a native module — breaks the zero-native-deps
  goal. Possible optional addon later.

## Floor

The native full-resolution capture of a single 2560×1440 display is ~25 ms warm;
that's effectively the floor without a persistent stream. Cursor-first reaches it
for the common case.
