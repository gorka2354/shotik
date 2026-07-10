'use strict';
// Screen/region recording session. A hidden recorder window captures + encodes;
// this module wires its chunks to a file, manages the border/controls windows,
// and registers the result in history.
const { BrowserWindow, desktopCapturer, screen, app, ipcMain } = require('electron');
const { EventEmitter } = require('events');
const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');

const GHOST = process.env.SHOTIK_GHOST === '1';
const TEST = process.env.SHOTIK_TEST === '1';
const GHOST_OFFSET = 20000;

const events = new EventEmitter();

let session = null; // { recWin, borderWin, controlsWin, stream, file, ext, firstFrame, startTs, paused, settled }

function isRecording() { return !!session; }
function status() {
  if (!session) return { recording: false };
  return { recording: true, paused: session.paused, file: session.file, startTs: session.startTs };
}

function tsName() {
  const d = new Date();
  const p = (n) => String(n).padStart(2, '0');
  return `Rec_${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}_${p(d.getHours())}-${p(d.getMinutes())}-${p(d.getSeconds())}`;
}

async function sourceIdFor(displayId) {
  if (TEST) return 'test';
  const sources = await desktopCapturer.getSources({ types: ['screen'], thumbnailSize: { width: 1, height: 1 } });
  const src = sources.find((s) => s.display_id === String(displayId)) || sources[0];
  return src ? src.id : '';
}

// deps injected from main.js to avoid circular requires
let deps = { history: null, windows: null, settings: null, t: (k) => k };
function init(d) { deps = { ...deps, ...d }; }

// startRecording({ displayId, rectPhys:{x,y,w,h}, rectDip:{x,y,w,h} }, opts)
async function startRecording({ displayId, rectPhys, rectDip }, opts = {}) {
  if (session) return null;
  const display = screen.getAllDisplays().find((d) => d.id === Number(displayId)) || screen.getPrimaryDisplay();
  const physW = Math.round(display.size.width * display.scaleFactor);
  const physH = Math.round(display.size.height * display.scaleFactor);
  const saveDir = deps.settings.get().saveDir;
  fs.mkdirSync(saveDir, { recursive: true });
  const partFile = path.join(app.getPath('temp'), `shotik-rec-${Date.now()}.part`);

  const st = deps.settings.get();
  const rec = st.recording || {};
  const sourceId = await sourceIdFor(displayId);

  // optional 3-2-1 countdown before capture actually begins
  if (rec.countdown && !GHOST && !TEST) {
    const cd = new BrowserWindow({
      x: Math.round(display.bounds.x + rectDip.x + rectDip.w / 2 - 90),
      y: Math.round(display.bounds.y + rectDip.y + rectDip.h / 2 - 90),
      width: 180, height: 180, frame: false, transparent: true, resizable: false,
      movable: false, focusable: false, skipTaskbar: true, hasShadow: false, alwaysOnTop: true,
      webPreferences: { contextIsolation: true },
    });
    cd.removeMenu();
    cd.setIgnoreMouseEvents(true);
    cd.setAlwaysOnTop(true, 'screen-saver');
    cd.loadFile(path.join(__dirname, '..', 'record', 'countdown.html'));
    await new Promise((r) => setTimeout(r, 2050));
    try { if (!cd.isDestroyed()) cd.destroy(); } catch (_) {}
  }

  const recWin = new BrowserWindow({
    width: 200, height: 200, x: GHOST_OFFSET, y: 200, show: false,
    frame: false, skipTaskbar: true,
    webPreferences: {
      preload: path.join(__dirname, '..', 'recorder', 'preload.js'),
      contextIsolation: true, nodeIntegration: false, backgroundThrottling: false,
    },
  });
  recWin.removeMenu();

  const stream = fs.createWriteStream(partFile);
  session = {
    recWin, borderWin: null, controlsWin: null, stream, file: null, ext: 'webm',
    partFile, firstFrame: null, startTs: Date.now(), paused: false, settled: false,
    displayId: Number(displayId), rectDip, saveBase: path.join(saveDir, tsName()),
  };

  recWin.loadFile(path.join(__dirname, '..', 'recorder', 'recorder.html'), {
    query: {
      test: TEST ? '1' : '0', sourceId,
      pw: String(physW), ph: String(physH),
      x: String(rectPhys.x), y: String(rectPhys.y), w: String(rectPhys.w), h: String(rectPhys.h),
      fps: String(rec.fps || 30), audio: rec.systemAudio ? '1' : '0', format: rec.format || 'auto',
    },
  });

  if (!GHOST) buildOverlayWindows(display, rectDip);
  events.emit('state', status());
  return status();
}

function buildOverlayWindows(display, rectDip) {
  // region border (just outside the crop, so it isn't recorded)
  const gx = display.bounds.x, gy = display.bounds.y;
  const b = { x: gx + rectDip.x - 3, y: gy + rectDip.y - 3, w: rectDip.w + 6, h: rectDip.h + 6 };
  const borderWin = new BrowserWindow({
    x: Math.round(b.x), y: Math.round(b.y), width: Math.round(b.w), height: Math.round(b.h),
    frame: false, transparent: true, resizable: false, movable: false, focusable: false,
    skipTaskbar: true, hasShadow: false, alwaysOnTop: true, enableLargerThanScreen: true,
    webPreferences: { contextIsolation: true },
  });
  borderWin.removeMenu();
  borderWin.setIgnoreMouseEvents(true);
  borderWin.setAlwaysOnTop(true, 'screen-saver');
  borderWin.loadFile(path.join(__dirname, '..', 'record', 'border.html'));
  session.borderWin = borderWin;

  // controls bar, docked just below the region (outside the crop)
  const cw = 240, ch = 48;
  let cx = gx + rectDip.x + rectDip.w / 2 - cw / 2;
  let cy = gy + rectDip.y + rectDip.h + 10;
  if (cy + ch > display.bounds.y + display.bounds.height - 4) cy = gy + rectDip.y - ch - 10;
  const controlsWin = new BrowserWindow({
    x: Math.round(cx), y: Math.round(cy), width: cw, height: ch,
    frame: false, transparent: true, resizable: false, movable: false,
    skipTaskbar: true, hasShadow: false, alwaysOnTop: true, minimizable: false, maximizable: false,
    webPreferences: {
      preload: path.join(__dirname, '..', 'record', 'preload.js'),
      contextIsolation: true, nodeIntegration: false,
    },
  });
  controlsWin.removeMenu();
  controlsWin.setAlwaysOnTop(true, 'screen-saver');
  controlsWin.loadFile(path.join(__dirname, '..', 'record', 'controls.html'));
  session.controlsWin = controlsWin;
}

function sendControl(cmd) {
  if (session && session.recWin && !session.recWin.isDestroyed()) session.recWin.webContents.send('rec:control', cmd);
}

function stopRecording() {
  if (!session || session.settled) return;
  session.stopping = true;
  sendControl('stop');
  // safety: if recorder never reports done, finalize after a grace period
  setTimeout(() => { if (session && session.stopping && !session.settled) finalize({ duration: (Date.now() - session.startTs) / 1000 }); }, 4000);
}
function pauseRecording() { if (session && !session.paused) { session.paused = true; sendControl('pause'); updateControls(); events.emit('state', status()); } }
function resumeRecording() { if (session && session.paused) { session.paused = false; sendControl('resume'); updateControls(); events.emit('state', status()); } }
function cancelRecording() {
  if (!session) return;
  session.canceled = true;
  sendControl('stop');
  setTimeout(() => { if (session && !session.settled) finalize({ duration: 0 }); }, 500);
}

function updateControls() {
  if (session && session.controlsWin && !session.controlsWin.isDestroyed()) {
    session.controlsWin.webContents.send('rec:state', { paused: session.paused });
  }
}

function finalize(info) {
  if (!session || session.settled) return;
  session.settled = true;
  const s = session;
  session = null;
  try { s.stream.end(); } catch (_) {}

  const cleanup = () => {
    for (const w of [s.recWin, s.borderWin, s.controlsWin]) { try { if (w && !w.isDestroyed()) w.destroy(); } catch (_) {} }
  };

  setTimeout(() => {
    if (s.canceled) {
      try { fs.unlinkSync(s.partFile); } catch (_) {}
      cleanup();
      events.emit('state', { recording: false });
      return;
    }
    const finalFile = `${s.saveBase}.${s.ext}`;
    let out = finalFile;
    try {
      for (let i = 2; fs.existsSync(out); i++) out = `${s.saveBase}-${i}.${s.ext}`;
      fs.renameSync(s.partFile, out);
    } catch (e) {
      try { fs.copyFileSync(s.partFile, out); fs.unlinkSync(s.partFile); } catch (_) {}
    }
    const entry = deps.history.addVideo({
      file: out, thumbBuf: s.firstFrame,
      width: s.rectDip ? Math.round(s.rectDip.w) : 0, height: s.rectDip ? Math.round(s.rectDip.h) : 0,
      duration: info.duration || 0,
    });
    cleanup();
    events.emit('done', entry);
    events.emit('state', { recording: false });
  }, 150);
}

// ---- IPC from the recorder window ----
ipcMain.on('rec:ready', (_e, info) => { if (session) { session.ext = (info && info.ext) || 'webm'; events.emit('ready'); } });
ipcMain.on('rec:chunk', (_e, chunk) => { if (session && session.stream.writable) { try { session.stream.write(Buffer.from(chunk)); } catch (_) {} } });
ipcMain.on('rec:first-frame', (_e, buf) => { if (session) session.firstFrame = Buffer.from(buf); });
ipcMain.on('rec:done', (_e, info) => finalize(info || {}));
ipcMain.on('rec:error', (_e, msg) => { events.emit('error', msg); if (session) { session.canceled = true; finalize({ duration: 0 }); } });
ipcMain.on('rec:controls-stop', () => stopRecording());
ipcMain.on('rec:controls-pause', () => (session && session.paused ? resumeRecording() : pauseRecording()));
ipcMain.on('rec:controls-cancel', () => cancelRecording());

module.exports = { init, events, startRecording, stopRecording, pauseRecording, resumeRecording, cancelRecording, isRecording, status };
