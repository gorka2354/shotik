# Performance

`bench/run.js` runs one scenario against any build; `bench/compare.js` prints the difference. The raw results
are in `bench/results/`.

| | Electron 1.3.0 | Electron 1.3.1 | Rust 2.0 |
|---|---:|---:|---:|
| Installer | 71 MB | 71 MB | 7 MB |
| Installed size | 269 MB | 269 MB | 27 MB |
| Startup to ready | 662 ms | 465 ms | 32 ms |
| Processes in the tray | 6 | 6 | 1 |
| Memory in the tray | 464 MB | 404 MB | 3 MB |
| CPU in the tray | 13 ms/s | 2 ms/s | 0 ms/s |
| Overlay shown | 753 ms | 6 460 ms | 82 ms |
| Memory with overlays open | 1 075 MB | 6 163 MB | 140 MB |
| Memory after closing them | 706 MB | 6 072 MB | 6 MB |
| `take_screenshot` | 1 677 ms | 3 804 ms | 191 ms |
| `list_windows` | 586 ms | 1 211 ms | 27 ms |
| `capture_window` | 999 ms | 1 714 ms | 31 ms |
| OCR of a selection | 685 ms | 698 ms | 49 ms |
| Video: start | 2 507 ms | 11 591 ms | 368 ms |
| Video: CPU | 871 ms/s | 617 ms/s | 46 ms/s |
| Video: peak memory | 1 514 MB | 6 782 MB | 113 MB |
| GIF export | 12 745 ms | 12 535 ms | 1 392 ms |
| Audio: start | 708 ms | 644 ms | 192 ms |
| Audio: CPU | 43 ms/s | 37 ms/s | 25 ms/s |

1.3.0 was measured with a game running, 1.3.1 and 2.0 on an idle machine, one after the other. 1.3.1 gave up its warm capture timer to fix a handle leak, which is why it captures slower than 1.3.0. Medians on a Ryzen 9 7950X3D with three monitors, one of them 4K at 150%, in ghost mode: on the real desktop a Rust overlay appears about 150 ms after the key press.

A recording costs size for speed: a minute of the benchmark's still screen is
12 MB of hardware H.264 against 6 MB of VP9, which
Chromium encoded in software at 617 ms of CPU per second.

## Why the Electron version was slow

Most of an overlay's time was `desktopCapturer.getSources`, which captures every screen on each call and starts
its engine cold. Keeping the engine warm on a timer took 350 ms off the overlay, but each call left two handles
behind inside Chromium's capturer: 140,000 handles and 5 GB of memory after 16 days.

## The rule that keeps Rust flat

GDI `BitBlt` takes a frame in tens of milliseconds and holds nothing between frames. Only Windows Graphics Capture,
for video, needs a D3D11 device, and on this driver releasing a device that drove capture leaks a section, a
mutant and about 123 KB per cycle. So a device that drove capture is never released, not even a lost one.

The cost: after the first video recording the process keeps about 100 MB of private memory, 42 threads, an NVENC
session and 25 MB of GPU memory until it exits. After audio-only recording it keeps about 3 MB. If OBS or
ShadowPlay run out of NVENC sessions, restarting Shotik gives its session back.

## Checking for a leak

```bash
node bench/soak.js --impl rust --exe target/release/shotik.exe --hours 6 --out soak.jsonl
```

Two instances run side by side, one idle and one taking a real overlay and a real screenshot every cycle. Each
sample has private memory and handle counts. Watch the handles: a leak of a few KB per call hides in the noise of
private bytes, and that is how this one stayed unseen for months.
