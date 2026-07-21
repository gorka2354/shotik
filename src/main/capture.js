'use strict';
// Screen grabbing + overlay (region selection) session management.
const { BrowserWindow, desktopCapturer, screen, app, ipcMain, nativeImage } = require('electron');
const { EventEmitter } = require('events');
const { execFile } = require('child_process');
const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');

// Ghost mode: windows are placed far off-screen and the screen is replaced by a
// fixture image — full e2e testing without disturbing the user's desktop.
const GHOST = process.env.SHOTIK_GHOST === '1';
const FAKE_SCREEN = process.env.SHOTIK_FAKE_SCREEN || null;
const GHOST_OFFSET = 20000;

const settings = require('./settings');

const events = new EventEmitter();

let session = null; // { resolve, wins: BrowserWindow[], files: string[] }
let lastRegion = undefined; // { displayId, rectPhys: {x,y,width,height} }; lazy-loaded from settings
let lastOverlayTiming = []; // step-by-step ms for the cursor overlay (perf diagnostics)
function getOverlayTiming() { return lastOverlayTiming; }
const { performance } = require('perf_hooks');
// Always record the last capture's timing to a file so real-machine latency can
// be inspected without a debug build (overwrite — no growth). Path in temp.
function flushTiming(timing) {
  lastOverlayTiming = timing.slice();
  try { fs.writeFileSync(path.join(app.getPath('temp'), 'shotik-overlay-timing.log'), timing.join(' ')); } catch (_) {}
}

function getLastRegion() {
  if (lastRegion === undefined) lastRegion = settings.get().lastRegion || null;
  return lastRegion;
}

function tempDir() {
  const dir = path.join(app.getPath('userData'), 'temp');
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

// Grab one display at full physical resolution. Returns NativeImage.
async function grabDisplay(display) {
  const phys = {
    width: Math.round(display.size.width * display.scaleFactor),
    height: Math.round(display.size.height * display.scaleFactor),
  };
  if (FAKE_SCREEN) {
    const img = nativeImage.createFromPath(FAKE_SCREEN);
    const sz = img.getSize();
    return (sz.width === phys.width && sz.height === phys.height)
      ? img
      : img.resize({ width: phys.width, height: phys.height, quality: 'best' });
  }
  const sources = await desktopCapturer.getSources({ types: ['screen'], thumbnailSize: phys });
  if (!sources.length) throw new Error('No screen sources available');
  const src = sources.find((s) => s.display_id === String(display.id)) || sources[0];
  return src.thumbnail;
}

// Visible top-level windows (physical px, z-order top first) for hover-to-snap.
// Resolved asynchronously so the overlay opens instantly and snapping arrives
// a moment later.
function enumWindows() {
  return new Promise((resolve) => {
    if (process.platform !== 'win32' && !FAKE_SCREEN) {
      resolve([]); // hover-snap is Windows-only for now — overlay degrades gracefully
      return;
    }
    if (FAKE_SCREEN) {
      // matches the window drawn in test/fake-screen.png (1920x1080 fixture
      // stretched onto the primary display)
      const d = screen.getPrimaryDisplay();
      const k = (d.size.width * d.scaleFactor) / 1920;
      resolve([
        { x: Math.round(240 * k), y: Math.round(140 * k), w: Math.round(1100 * k), h: Math.round(640 * k) },
        { x: Math.round(1470 * k), y: Math.round(170 * k), w: Math.round(180 * k), h: Math.round(60 * k) },
      ]);
      return;
    }
    const script = path.join(__dirname, 'windows-enum.ps1')
      .replace('app.asar' + path.sep, 'app.asar.unpacked' + path.sep);
    execFile('powershell.exe',
      ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
        '-File', script, '-ExcludePid', String(process.pid)],
      { timeout: 8000, windowsHide: true },
      (err, stdout) => {
        if (err) return resolve([]);
        try { resolve(JSON.parse(String(stdout).trim() || '[]')); } catch (_) { resolve([]); }
      });
  });
}

// The first desktopCapturer.getSources in a process is slow (~300-600ms) — it
// spins up the capture engine. Warming it at startup (and keeping it hot) makes
// the first real capture, when the user hits the hotkey, snappy.
let warmTimer = null;
async function prewarm() {
  try { await desktopCapturer.getSources({ types: ['screen'], thumbnailSize: { width: 1, height: 1 } }); } catch (_) {}
}
function keepWarm() {
  if (FAKE_SCREEN) return;
  prewarm();
  clearInterval(warmTimer);
  warmTimer = setInterval(prewarm, 30000);
}

function displayById(id) {
  return screen.getAllDisplays().find((d) => d.id === id) || screen.getPrimaryDisplay();
}

function displayUnderCursor() {
  return screen.getDisplayNearestPoint(screen.getCursorScreenPoint());
}

// Full screen capture of a display -> { png: Buffer, display }
async function captureFullscreen(displayId = null) {
  const display = displayId ? displayById(displayId) : displayUnderCursor();
  const img = await grabDisplay(display);
  return { png: img.toPNG(), display, image: img };
}

// Re-capture the exact same region as last time (fresh pixels). null if no last region.
async function captureLastRegion() {
  const lr = getLastRegion();
  if (!lr) return null;
  const display = displayById(lr.displayId);
  const img = await grabDisplay(display);
  const size = img.getSize();
  const rect = {
    x: Math.min(lr.rectPhys.x, size.width - 1),
    y: Math.min(lr.rectPhys.y, size.height - 1),
    width: Math.min(lr.rectPhys.width, size.width - lr.rectPhys.x),
    height: Math.min(lr.rectPhys.height, size.height - lr.rectPhys.y),
  };
  if (rect.width < 1 || rect.height < 1) return null;
  const cropped = img.crop(rect);
  return { png: cropped.toPNG(), display, rectPhys: lr.rectPhys };
}

function setLastRegion(displayId, rectPhys) {
  lastRegion = { displayId, rectPhys };
  try { settings.set({ lastRegion }); } catch (_) {}
}
function hasLastRegion() { return !!getLastRegion(); }

// Capture an arbitrary region (DIP coords relative to a display). For MCP.
async function captureRegion(displayId, rectDip) {
  const display = displayId ? displayById(displayId) : screen.getPrimaryDisplay();
  const img = await grabDisplay(display);
  const s = display.scaleFactor;
  const size = img.getSize();
  const rect = {
    x: Math.max(0, Math.round(rectDip.x * s)),
    y: Math.max(0, Math.round(rectDip.y * s)),
    width: Math.round(rectDip.width * s),
    height: Math.round(rectDip.height * s),
  };
  rect.width = Math.min(rect.width, size.width - rect.x);
  rect.height = Math.min(rect.height, size.height - rect.y);
  if (rect.width < 1 || rect.height < 1) throw new Error('Region outside display bounds');
  return { png: img.crop(rect).toPNG(), display };
}

// Opens the freeze-frame overlay on every display. Resolves with:
//   { action, png: Buffer|null, rectCss, rectImg, displayId, screenRect } or null if cancelled.
function startOverlaySession(opts = {}) {
  if (session) {
    // Bring existing overlay forward instead of stacking sessions.
    for (const w of session.wins) if (!w.isDestroyed()) w.focus();
    return Promise.resolve(undefined); // undefined = "was busy", caller ignores
  }
  return new Promise(async (resolve) => {
    const t0 = performance.now();
    const timing = [];
    const mark = (l) => { timing.push(`${l}=${Math.round(performance.now() - t0)}`); };
    const displays = screen.getAllDisplays();
    const cursorDisplay = displayUnderCursor();
    const wins = [];
    const files = [];
    const winInfos = []; // { win, display } — keyed pairs (wins[] order is nondeterministic under parallel capture)
    session = { resolve, wins, files, settled: false, winInfos };

    // Window rects for hover-snap arrive async; deliver to each overlay once
    // BOTH the rects and that window exist (order-independent — see PERF.md).
    let winRects = null;
    const deliver = (info) => {
      if (!winRects || info.delivered || !info.win || info.win.isDestroyed()) return;
      info.delivered = true;
      const d = info.display, sf = d.scaleFactor;
      const rel = winRects
        .map((r) => ({ x: r.x / sf - d.bounds.x, y: r.y / sf - d.bounds.y, w: r.w / sf, h: r.h / sf }))
        .filter((r) => r.x < d.bounds.width && r.y < d.bounds.height && r.x + r.w > 0 && r.y + r.h > 0);
      const send = () => { if (!info.win.isDestroyed()) info.win.webContents.send('overlay:windows', rel); };
      if (info.win.webContents.isLoading()) info.win.webContents.once('did-finish-load', send);
      else send();
    };
    enumWindows().then((r) => { if (!session || session.settled) return; winRects = r; winInfos.slice().forEach(deliver); });

    // getSources captures ALL screens per call, so per-display calls multiply the
    // cost. Capture the cursor display first with one call and show it right away
    // (~25ms warm); capture the other monitors natively in the background.
    async function makeOverlay(d, preImg) {
      const isCursor = d.id === cursorDisplay.id;
      const img = preImg || await grabDisplay(d);
      if (isCursor) mark('grab');
      const physSize = img.getSize();
      // raw BGRA bitmap handed to the overlay over IPC — no PNG encode/decode,
      // no disk round-trip (saves ~120ms/display + I/O)
      const bitmap = img.toBitmap();
      const bounds = {
        x: d.bounds.x + (GHOST ? GHOST_OFFSET : 0), y: d.bounds.y,
        width: d.bounds.width, height: d.bounds.height,
      };
      const win = new BrowserWindow({
        ...bounds,
        // resizable MUST be true on Windows: a non-resizable window is clamped
        // to the display work area and can't cover the taskbar. thickFrame:false
        // removes the resize border there, so the user still can't resize it.
        // On macOS thickFrame does nothing and a resizable frameless window CAN
        // be dragged at the edges — the frozen screenshot would stretch.
        frame: false, show: false, transparent: false, backgroundColor: '#0b0d11',
        skipTaskbar: true, resizable: process.platform === 'win32', thickFrame: false, movable: false,
        minimizable: false, maximizable: false, fullscreenable: false,
        enableLargerThanScreen: true, hasShadow: false, roundedCorners: false,
        webPreferences: {
          preload: path.join(__dirname, '..', 'overlay', 'preload.js'),
          contextIsolation: true, nodeIntegration: false, spellcheck: false,
          backgroundThrottling: false,
        },
      });
      win.removeMenu();
      // belt & braces on every platform: the overlay must never be resized by
      // the user (its canvas would stretch the frozen frame)
      win.on('will-resize', (e) => e.preventDefault());
      win.setBounds(bounds); // force exact size — creation may have clamped it
      win.loadFile(path.join(__dirname, '..', 'overlay', 'overlay.html'), {
        query: {
          displayId: String(d.id),
          scale: String(d.scaleFactor),
          physW: String(physSize.width),
          physH: String(physSize.height),
          focused: d.id === cursorDisplay.id ? '1' : '0',
          claude: opts.forClaude ? '1' : '0',
          text: opts.forText ? '1' : '0',
          record: opts.forRecord ? '1' : '0',
        },
      });
      if (isCursor) mark('winCreate');
      // Show ONLY after the renderer confirms the frame is painted — showing
      // right after the IPC send flashes the bare background for a frame
      // (the visible "blink" on capture). A watchdog still shows the window if
      // the renderer never reports (crash), so the session can't hang unseen.
      const showNow = (label) => {
        if (info.shown || win.isDestroyed()) return;
        info.shown = true;
        clearTimeout(info.showTimer);
        if (isCursor && label) mark(label);
        if (GHOST) {
          win.showInactive(); // painted but off-screen, never steals focus
          if (isCursor) { mark('show'); flushTiming(timing); }
          return;
        }
        win.show();
        win.setAlwaysOnTop(true, 'screen-saver');
        const b = win.getBounds();
        if (b.width !== bounds.width || b.height !== bounds.height || b.x !== bounds.x || b.y !== bounds.y) {
          win.setBounds(bounds);
        }
        if (isCursor) { win.focus(); mark('show'); flushTiming(timing); }
      };
      win.webContents.once('did-finish-load', () => {
        if (win.isDestroyed()) return;
        if (isCursor) mark('load');
        win.setBounds(bounds);
        win.webContents.send('overlay:frame', { bitmap, width: physSize.width, height: physSize.height });
        if (isCursor) mark('frameSent');
        info.showTimer = setTimeout(() => showNow('paintTimeout'), 800); // watchdog
      });
      win.on('closed', () => {
        if (session && !session.settled) settle(null);
      });
      const info = { win, display: d, delivered: false, shown: false, showTimer: null, showNow, isCursor };
      wins.push(win);
      winInfos.push(info);
      deliver(info); // in case window rects are already in
    }

    try {
      // cursor display first — capture + show it immediately (perceived latency)
      await makeOverlay(cursorDisplay);
      // other monitors natively, in the background (don't block the active one)
      const others = displays.filter((d) => d.id !== cursorDisplay.id);
      Promise.all(others.map((d) => makeOverlay(d).catch((e) => console.error('overlay display failed', e))));
    } catch (e) {
      console.error('overlay session failed', e);
      settle(null);
      return;
    }
  });
}

function settle(result) {
  if (!session || session.settled) return;
  session.settled = true;
  const { resolve, wins, files } = session;
  session = null;
  for (const w of wins) { try { if (!w.isDestroyed()) w.destroy(); } catch (_) {} }
  setTimeout(() => { for (const f of files) { try { fs.unlinkSync(f); } catch (_) {} } }, 1000);
  resolve(result);
}

function isOverlayOpen() { return !!session; }

function overlayWindows() { return session ? session.wins : []; }

// ---- IPC from overlay renderers ----
// The renderer reports "frame is on screen" — only then is its window shown
// (kills the one-frame background flash on capture).
ipcMain.on('overlay:painted', (e) => {
  if (!session || !session.winInfos) return;
  const info = session.winInfos.find((i) => i.win && !i.win.isDestroyed() && i.win.webContents === e.sender);
  if (info) info.showNow('painted');
});

ipcMain.handle('overlay:action', (_e, payload) => {
  const png = payload.png ? Buffer.from(payload.png) : null;
  settle({ ...payload, png });
});
ipcMain.on('overlay:cancel', () => settle(null));
ipcMain.on('overlay:color', (_e, hex) => events.emit('color-copied', hex));

module.exports = {
  events, grabDisplay, captureFullscreen, captureRegion, captureLastRegion,
  setLastRegion, hasLastRegion, startOverlaySession, isOverlayOpen, overlayWindows,
  displayById, displayUnderCursor, keepWarm, prewarm, getOverlayTiming,
};
