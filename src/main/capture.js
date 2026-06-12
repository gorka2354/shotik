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
    const displays = screen.getAllDisplays();
    const cursorDisplay = displayUnderCursor();
    const wins = [];
    const files = [];
    session = { resolve, wins, files, settled: false };

    try {
      for (const d of displays) {
        const img = await grabDisplay(d);
        const file = path.join(tempDir(), `freeze-${d.id}-${Date.now()}.png`);
        fs.writeFileSync(file, img.toPNG());
        files.push(file);
        const physSize = img.getSize();

        const bounds = {
          x: d.bounds.x + (GHOST ? GHOST_OFFSET : 0), y: d.bounds.y,
          width: d.bounds.width, height: d.bounds.height,
        };
        const win = new BrowserWindow({
          ...bounds,
          // resizable MUST be true: on Windows a non-resizable window is clamped
          // to the display work area and can't cover the taskbar. thickFrame:false
          // removes the resize border, so the user still can't resize it.
          frame: false, show: false, transparent: false, backgroundColor: '#0b0d11',
          skipTaskbar: true, resizable: true, thickFrame: false, movable: false,
          minimizable: false, maximizable: false, fullscreenable: false,
          enableLargerThanScreen: true, hasShadow: false, roundedCorners: false,
          webPreferences: {
            preload: path.join(__dirname, '..', 'overlay', 'preload.js'),
            contextIsolation: true, nodeIntegration: false, spellcheck: false,
            backgroundThrottling: false,
          },
        });
        win.removeMenu();
        win.setBounds(bounds); // force exact size — creation may have clamped it
        win.loadFile(path.join(__dirname, '..', 'overlay', 'overlay.html'), {
          query: {
            img: pathToFileURL(file).href,
            displayId: String(d.id),
            scale: String(d.scaleFactor),
            physW: String(physSize.width),
            physH: String(physSize.height),
            focused: d.id === cursorDisplay.id ? '1' : '0',
            claude: opts.forClaude ? '1' : '0',
          },
        });
        win.webContents.once('did-finish-load', () => {
          if (win.isDestroyed()) return;
          win.setBounds(bounds);
          if (GHOST) {
            win.showInactive(); // painted but off-screen, never steals focus
            return;
          }
          win.show();
          win.setAlwaysOnTop(true, 'screen-saver');
          // showing can re-clamp the bounds — verify and force once more
          const b = win.getBounds();
          if (b.width !== bounds.width || b.height !== bounds.height || b.x !== bounds.x || b.y !== bounds.y) {
            win.setBounds(bounds);
          }
          if (d.id === cursorDisplay.id) win.focus();
        });
        win.on('closed', () => {
          // If a window dies unexpectedly, cancel the whole session.
          if (session && !session.settled) settle(null);
        });
        wins.push(win);
      }
    } catch (e) {
      console.error('overlay session failed', e);
      settle(null);
      return;
    }

    // Deliver window rects for hover-to-snap (display-relative DIP coords).
    // Note: the phys->DIP conversion assumes a uniform scale factor per
    // display, which is exact on single-scale setups.
    enumWindows().then((winRects) => {
      if (!session || session.settled) return;
      displays.forEach((d, i) => {
        const w = wins[i];
        if (!w || w.isDestroyed()) return;
        const sf = d.scaleFactor;
        const rel = winRects
          .map((r) => ({
            x: r.x / sf - d.bounds.x, y: r.y / sf - d.bounds.y,
            w: r.w / sf, h: r.h / sf,
          }))
          .filter((r) => r.x < d.bounds.width && r.y < d.bounds.height && r.x + r.w > 0 && r.y + r.h > 0);
        const deliver = () => { if (!w.isDestroyed()) w.webContents.send('overlay:windows', rel); };
        // the renderer may not have registered its listener yet
        if (w.webContents.isLoading()) w.webContents.once('did-finish-load', deliver);
        else deliver();
      });
    });
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
ipcMain.handle('overlay:action', (_e, payload) => {
  const png = payload.png ? Buffer.from(payload.png) : null;
  settle({ ...payload, png });
});
ipcMain.on('overlay:cancel', () => settle(null));
ipcMain.on('overlay:color', (_e, hex) => events.emit('color-copied', hex));

module.exports = {
  events, grabDisplay, captureFullscreen, captureRegion, captureLastRegion,
  setLastRegion, hasLastRegion, startOverlaySession, isOverlayOpen, overlayWindows,
  displayById, displayUnderCursor,
};
