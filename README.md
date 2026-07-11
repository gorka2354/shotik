<p align="center">
  <img src="assets/icon.png" width="96" alt="Shotik">
</p>

<h1 align="center">Shotik</h1>

<p align="center">
  <b>Screenshots &amp; translation for humans and AI</b> · a beautiful open-source utility for Windows<br>
  with a built-in MCP server (Claude can see your screen) and translate-anything-on-screen
</p>

<p align="center">
  <a href="README.ru.md">Русская версия →</a>
</p>

<p align="center">
  <a href="https://github.com/gorka2354/shotik/releases/latest"><img src="https://img.shields.io/github/v/release/gorka2354/shotik?label=release&color=2ea44f" alt="Release"></a>
  <a href="https://github.com/gorka2354/shotik/releases"><img src="https://img.shields.io/github/downloads/gorka2354/shotik/total?color=blue" alt="Downloads"></a>
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11-0078d4" alt="Platform">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-lightgrey" alt="MIT"></a>
  <img src="https://img.shields.io/badge/MCP-built--in-7c5cff" alt="MCP">
  <img src="https://img.shields.io/badge/deps-Electron%20only-informational" alt="Zero deps">
</p>

---

<p align="center">
  <img src="docs/promo.gif" width="88%" alt="Shotik: capture → annotate → OCR → one keypress to Claude">
</p>
<p align="center"><i>capture → annotate → OCR → one keypress to Claude · <a href="docs/promo.mp4">full-quality mp4</a></i></p>

<p align="center">
  <img src="docs/app-dark.png" width="46%" alt="Dark theme">
  <img src="docs/app-light.png" width="46%" alt="Light theme">
</p>
<p align="center"><i>native look — follows your system theme and Windows accent color · English &amp; Russian</i></p>

## Why Shotik?

Most screenshots today don't go to a photo album — they go to **Claude/ChatGPT in a console**, or to a chat, or you need the **text inside them**. Shotik is built for that world:

- **AI-first.** One keypress puts a shot straight into Claude Code, correctly. A built-in MCP server lets Claude *take* screenshots itself.
- **Translate anything you can see.** Select text in any app — or even text baked into a Telegram post, a Steam page, an image or a video — and get an instant translation. No copy-paste, no browser tab.
- **Speed without dialogs.** Capture → it's already in the clipboard, already on disk, ready to paste.
- **Lightweight & honest.** One runtime dependency (Electron). No telemetry, no account, no cloud. ~0.5% of one CPU core at idle (see [Performance](#performance--privacy)).

Think ShareX capture + Flameshot annotations + a DeepL-style translator + Claude, in one small MIT app.

## ✨ New in 1.0 — translate anything on screen

<p align="center">
  <img src="docs/translate-popup.png" width="330" alt="Translate popup next to the cursor">
  &nbsp;&nbsp;
  <img src="docs/translate-bubble.png" width="52" alt="Translate bubble on selection" align="top">
</p>

**Just select text — a translate bubble pops up next to it; click, and the translation appears.** It works three ways, cleanest first:

1. **UI Automation** — in browsers, editors, PDFs, chat inputs: read instantly, *never touches your clipboard*.
2. **On-screen OCR** — in apps that draw their own text (**Telegram posts, Steam**, images, video frames): Shotik finds the selection highlight and reads the pixels you selected. Still no clipboard.
3. **`Ctrl+Alt+T`** — a manual shortcut that translates the current selection anywhere.

Prefer typing? The new **Text** tab is a built-in translator — paste text, pick a language, done.

<p align="center">
  <img src="docs/app-text-light.png" width="88%" alt="The Text tab: quick translate + how-to">
</p>

Translation is **free out of the box** (no key) or via your own **DeepL** key. A genuine replacement for the DeepL desktop app — and it reaches text DeepL can't (custom-drawn apps, images, video).

## Features

### ✦ Smart Clipboard
After every capture the clipboard holds **both formats at once**: the PNG image and the file path as text.
`Ctrl+V` into Claude Code pastes the image; `Ctrl+V` into a terminal pastes the path. No switching.

### ✦ Built-in MCP server
Connect once:

```bash
claude mcp add shotik --transport http http://127.0.0.1:7464/mcp
```

and Claude gets these tools:

| Tool | What it does |
|---|---|
| `take_screenshot` | look at the screen right now |
| `ask_user_to_select_region` | open the overlay so **you** point at the area |
| `get_last_screenshot` | grab your latest shot |
| `take_screenshot_region` | capture a specific rectangle |
| `list_screenshots`, `list_displays` | history and monitors |

Say "look at my screen" — and Claude sees it. Say "look here" — and just draw a box with your mouse.

### ✦ Translate selected text — no screenshot *(1.0)*
Select text in **any** app and a translate bubble appears; click it for the translation. Reads the selection through Windows UI Automation (no clipboard) — and via on-screen OCR with **highlight detection** in Telegram, Steam, images and video. Toggle in **Settings → Text & translation**; `Ctrl+Alt+T` and the **Text** tab work too.

### ✦ Live Text — grab &amp; translate text from anything (`Ctrl+Shift+PrtSc`)
Select an area and the recognized words become **selectable, right on the frozen screen** — like macOS Live Text. Drag to grab words, `Enter` to copy, or hit **Translate**. Offline OCR (your Windows languages). Better than PowerToys Text Extractor — that one just dumps everything into the clipboard.

### ✦ Flameshot-style overlay with annotations
Freeze-frame, magnifier with a pixel-perfect HEX picker, boxes, arrows, pen, highlighter,
**pixelate your secrets**, text, numbered markers — all inside the overlay, before saving.
Hovering highlights the window under the cursor; click captures it (hold `Alt` to disable snapping,
`Shift`+click for the whole screen).

### ✦ Repeat last area (`Shift+PrtSc`)
Same region, fresh pixels, instant, no overlay. Perfect for iteration loops:
fix the CSS → `Shift+PrtSc` → `Ctrl+V` to Claude → "see it now?". Survives restarts.

### ✦ Screen recording → video or GIF (`Ctrl+Alt+PrtSc`)
Select a region (or a window, or the whole screen) and record it to **WebM**, with a floating
controls bar (timer, pause, stop) and a live red border. Then **export any recording to GIF** in one
click. Optional system-audio capture, 15–60 fps and a 3-2-1 countdown.

### ✦ PowerToys Command Palette / Win+S
The installer adds Start Menu commands that show up in PowerToys Command Palette and Windows search:
**Area Screenshot**, **Full Screen**, **Repeat Last Area**, **Translate Selection**.
CLI too: `Shotik.exe --capture region|full|repeat|text|record|translate`.

### ✦ And the rest
- **Pin** a shot on top of every window exactly where you cut it (wheel = zoom, right-click = menu)
- **Color picker** — `C` in the overlay copies the pixel HEX
- History gallery, toasts with previews, multi-monitor, HiDPI
- **Native look**: light/dark theme and accent color come from the OS and switch live
- **English &amp; Russian** UI (follows the system language, switchable in Settings)

## Hotkeys

| Action | Windows | macOS |
|---|---|---|
| Capture area | `PrtSc` | `⌘⇧2` |
| Full screen (monitor under cursor) | `Ctrl+PrtSc` | `⌘⇧1` |
| Repeat last area | `Shift+PrtSc` | `⌘⇧7` |
| Extract text (Live Text) | `Ctrl+Shift+PrtSc` | `⌘⇧4` |
| Record region → video / GIF | `Ctrl+Alt+PrtSc` | `⌘⇧6` |
| Translate selected text | `Ctrl+Alt+T` | `⌘⌥T` |

In the overlay: `Enter`/double-click — copy · `Ctrl+S` — save as · `P` — pin · `T` — OCR ·
`A` — copy for Claude · `F` — whole screen · `1..0` — tools · `[` `]` — stroke width ·
`Ctrl+Z` — undo · arrows — nudge selection · `Esc` — cancel.

## Install

**Prebuilt:** [Releases](https://github.com/gorka2354/shotik/releases) →
`Shotik-Setup-x.x.x.exe` (installer with shortcuts) or `Shotik-x.x.x-portable.exe` (single file, no install).

**From source:**

```bash
git clone https://github.com/gorka2354/shotik && cd shotik
npm install
npm start
```

Windows 10/11 (Node.js ≥ 20 to build). The app lives in the tray; quit via the tray menu.

**macOS (experimental):** `.dmg` builds (arm64 &amp; x64) are produced in Releases. OCR uses Apple Vision;
window hover-snap and the translate bubble are Windows-only for now. Builds are unsigned:
right-click → Open on first launch, and grant Screen Recording permission.

**Microsoft Store:** packaging is ready (`npm run dist:store` builds the MSIX) — see [docs/STORE.md](docs/STORE.md).

## Performance &amp; privacy

Shotik is meant to disappear into the tray and cost nothing while it's there.

- **Idle CPU ≈ 0.5% of one core** — effectively nothing. Capture/record work only runs on demand.
- **No telemetry, no account, no network** except the translation call you trigger (free service or your DeepL key). Screenshots stay on your disk.
- **One runtime dependency: Electron.** No bundlers, no frameworks, no native modules.
- **The select-to-translate watcher** is a small background helper (Windows UI Automation) that reads *whether* text is selected — it never logs keystrokes and never touches your clipboard. It runs **only** while "Translate bubble on selection" is enabled, and you can turn it off in Settings. Its process is plainly labelled (`selection-watch.ps1`) and self-exits if Shotik closes — nothing hidden.

## For contributors: quality &amp; ghost mode

Tests never touch your desktop: windows are placed off-screen, the desktop is replaced by a fixture,
global hotkeys aren't registered, and everything is driven over HTTP `/test/*` endpoints.

```bash
npm test          # unit + ghost integration (translation, OCR, highlight detection, the bubble, the Text tab)
npm run test:unit # fast, no Electron
```

The suite covers the parts that are easy to get subtly wrong: OCR faithfulness (small text is upscaled;
Latin vs Cyrillic engines are disambiguated), selection-highlight detection, popup dismiss races, and
the translate flow end-to-end. The README demo is rendered from code with
[motion-engine](https://www.remotion.dev/) (Remotion).

## Architecture

```
src/
  main/        capture (desktopCapturer), history, OCR (WinRT / Apple Vision), translation,
               selection watcher + highlight detection, MCP server (zero-deps HTTP), windows, i18n
  overlay/     selection overlay: canvas, annotations, toolbar
  app/         main window: gallery, Claude page, Text tab, settings
  bubble/ translate-popup/ pin/ toast/   the small floating windows
```

## License

MIT
