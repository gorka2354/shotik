'use strict';
// Screen/region recording session. A hidden recorder window captures + encodes;
// this module wires its chunks to a file, manages the border/controls windows,
// and registers the result in history. Audio-only sessions (calls) skip the
// region and the border: system audio and/or microphone → .m4a.
const { BrowserWindow, desktopCapturer, screen, app, ipcMain } = require('electron');
const electronSession = require('electron').session;
const { EventEmitter } = require('events');
const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');
const { createMediaIndexer, writeIndex } = require('./media-index');

const GHOST = process.env.SHOTIK_GHOST === '1';
// SHOTIK_REAL_CAPTURE lets a test instance capture the real screen (instead of
// the synthetic source) — used for the one thing ghost can't verify.
const REAL_CAPTURE = process.env.SHOTIK_REAL_CAPTURE === '1';
// SHOTIK_FORCE_CONTROLS builds the border/controls windows even in ghost mode
// (placed off-screen) so their rendering + stop wiring can be verified.
const FORCE_CONTROLS = process.env.SHOTIK_FORCE_CONTROLS === '1';
const TEST = process.env.SHOTIK_TEST === '1' && !REAL_CAPTURE;
const GHOST_OFFSET = 20000;

const events = new EventEmitter();

let session = null; // { recWin, borderWin, controlsWin, stream, file, ext, firstFrame, startTs, paused, settled }
let trace = [];

function isRecording() { return !!session; }
function status() {
  if (!session) return { recording: false, trace };
  return { recording: true, audioOnly: session.audioOnly, paused: session.paused, audio: session.audio || null, levels: session.levels || null, file: session.file, startTs: session.startTs, trace, chunks: session.chunks || 0, bytes: session.bytes || 0 };
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

// startRecording({ displayId, rectPhys:{x,y,w,h}, rectDip:{x,y,w,h} }, opts) — region → video
// startRecording({ audioOnly: true }) — system audio and/or microphone → audio file
async function startRecording({ displayId, rectPhys, rectDip, audioOnly = false }, opts = {}) {
  if (session) return null;
  trace = [];
  const display = screen.getAllDisplays().find((d) => d.id === Number(displayId)) || screen.getPrimaryDisplay();
  const physW = Math.round(display.size.width * display.scaleFactor);
  const physH = Math.round(display.size.height * display.scaleFactor);
  const saveDir = deps.settings.get().saveDir;
  fs.mkdirSync(saveDir, { recursive: true });
  const partFile = path.join(app.getPath('temp'), `shotik-rec-${Date.now()}.part`);

  const st = deps.settings.get();
  const rec = st.recording || {};
  // inputs: video uses recording.*, audio mode its own toggles; the microphone device is shared
  const inputs = audioOnly ? (st.audioRecording || {}) : rec;
  const systemAudio = !!inputs.systemAudio, microphone = !!inputs.microphone;
  const sourceId = audioOnly ? '' : await sourceIdFor(displayId);

  // optional 3-2-1 countdown before capture actually begins (nothing to frame in audio mode)
  if (rec.countdown && !audioOnly && !GHOST && !TEST) {
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

  // Route the recorder window's getDisplayMedia() to the chosen screen (real capture).
  // Audio mode asks for the screen only as the carrier of system audio.
  if (!TEST && (!audioOnly || systemAudio)) {
    electronSession.defaultSession.setDisplayMediaRequestHandler(async (req, cb) => {
      try {
        const srcs = await desktopCapturer.getSources({ types: ['screen'], thumbnailSize: { width: 0, height: 0 } });
        const src = srcs.find((s) => s.display_id === String(displayId)) || srcs[0];
        cb({ video: src, audio: systemAudio ? 'loopback' : undefined });
      } catch (_) { cb({}); }
    }, { useSystemPicker: false });
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
  // getDisplayMedia needs a user gesture — executeJavaScript(..., true) provides it
  recWin.webContents.once('did-finish-load', () => {
    if (!recWin.isDestroyed()) recWin.webContents.executeJavaScript('window.__shotikStart && window.__shotikStart()', true).catch(() => {});
  });

  const stream = fs.createWriteStream(partFile);
  session = {
    recWin, borderWin: null, controlsWin: null, stream, indexer: createMediaIndexer(), file: null,
    ext: audioOnly ? 'm4a' : 'webm', audioOnly, inputs: { system: systemAudio, microphone }, levels: null,
    partFile, firstFrame: null, startTs: Date.now(), paused: false, settled: false,
    displayId: Number(displayId), rectDip: audioOnly ? null : rectDip, saveBase: path.join(saveDir, tsName()),
  };

  const common = {
    test: TEST ? '1' : '0', audio: systemAudio ? '1' : '0',
    microphone: microphone ? '1' : '0', microphoneDeviceId: rec.microphoneDeviceId || 'default',
  };
  recWin.loadFile(path.join(__dirname, '..', 'recorder', 'recorder.html'), {
    query: audioOnly ? { ...common, audioOnly: '1' } : {
      ...common, sourceId,
      pw: String(physW), ph: String(physH),
      x: String(rectPhys.x), y: String(rectPhys.y), w: String(rectPhys.w), h: String(rectPhys.h),
      fps: String(rec.fps || 30), format: rec.format || 'auto',
    },
  });

  if (!GHOST || FORCE_CONTROLS) {
    if (audioOnly) buildControls(display, null); else buildOverlayWindows(display, rectDip);
  }
  events.emit('state', status());
  return status();
}

function buildOverlayWindows(display, rectDip) {
  // region border (just outside the crop, so it isn't recorded)
  const off = GHOST ? GHOST_OFFSET : 0; // keep test windows off-screen
  const gx = display.bounds.x + off, gy = display.bounds.y;
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
  buildControls(display, rectDip);
}

// Controls bar (timer, level meters, pause/stop/cancel): docked just below the
// region (outside the crop) or, with no region (audio), above the taskbar.
function buildControls(display, rectDip) {
  const off = GHOST ? GHOST_OFFSET : 0; // keep test windows off-screen
  const meters = ['system', 'microphone'].filter((k) => session.inputs[k]);
  const cw = 240 + meters.length * 44, ch = 48;
  const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
  const dx = display.bounds.x + off, dy = display.bounds.y;
  let cx, cy;
  if (rectDip) {
    const gx = display.bounds.x + off, gy = display.bounds.y;
    cx = gx + rectDip.x + rectDip.w / 2 - cw / 2;
    cy = gy + rectDip.y + rectDip.h + 10;
    if (cy + ch > dy + display.bounds.height - 4) cy = gy + rectDip.y - ch - 10; // flip above
  } else {
    const wa = display.workArea;
    cx = wa.x + off + wa.width / 2 - cw / 2;
    cy = wa.y + wa.height - ch - 12;
  }
  cx = clamp(cx, dx + 4, dx + display.bounds.width - cw - 4);
  cy = clamp(cy, dy + 4, dy + display.bounds.height - ch - 4);
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
  controlsWin.loadFile(path.join(__dirname, '..', 'record', 'controls.html'), { query: { meters: meters.join(',') } });
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
  if (info.canceled) session.canceled = true;
  session.settled = true;
  const s = session;
  trace.push('finalize chunks=' + (s.chunks || 0) + ' bytes=' + (s.bytes || 0) + ' canceled=' + !!s.canceled + ' partExists=' + fs.existsSync(s.partFile));
  session = null;
  // Cues + Duration go to the end of the file, the rest is patched in place
  // once the stream is closed (MediaRecorder WebM is otherwise unseekable).
  const index = s.canceled ? null : s.indexer.finish();
  try {
    if (index && index.flush.length) s.stream.write(index.flush);
    if (index && index.append && index.append.length) s.stream.write(index.append);
    s.stream.end();
  } catch (_) {}
  const closed = new Promise((resolve) => {
    if (s.stream.closed) return resolve();
    s.stream.once('close', resolve);
    setTimeout(resolve, 5000); // a stuck stream must not lose the recording
  });

  const cleanup = () => {
    for (const w of [s.recWin, s.borderWin, s.controlsWin]) { try { if (w && !w.isDestroyed()) w.destroy(); } catch (_) {} }
    if (!TEST) { try { electronSession.defaultSession.setDisplayMediaRequestHandler(null); } catch (_) {} }
  };

  closed.then(() => {
    if (s.canceled) {
      try { fs.unlinkSync(s.partFile); } catch (_) {}
      cleanup();
      events.emit('state', { recording: false });
      return;
    }
    try {
      const problem = writeIndex(s.partFile, index);
      trace.push(problem ? 'index skipped: ' + problem : `index ok cues=${index.cues} duration=${index.duration}`);
    } catch (e) { trace.push('index failed: ' + e.message); }
    const finalFile = `${s.saveBase}.${s.ext}`;
    let out = finalFile;
    try {
      for (let i = 2; fs.existsSync(out); i++) out = `${s.saveBase}-${i}.${s.ext}`;
      fs.renameSync(s.partFile, out);
    } catch (e) {
      try { fs.copyFileSync(s.partFile, out); fs.unlinkSync(s.partFile); } catch (_) {}
    }
    const entry = deps.history.addVideo({
      file: out, thumbBuf: s.firstFrame, kind: s.audioOnly ? 'audio' : 'video',
      width: s.rectDip ? Math.round(s.rectDip.w) : 0, height: s.rectDip ? Math.round(s.rectDip.h) : 0,
      duration: info.duration || 0,
    });
    cleanup();
    events.emit('done', entry);
    events.emit('state', { recording: false });
  });
}

// ---- IPC from the recorder window ----
ipcMain.on('rec:ready', (_e, info) => { if (session) { session.ext = (info && info.ext) || 'webm'; session.audio = info && info.audio; events.emit('ready'); } });
ipcMain.on('rec:chunk', (_e, chunk) => { if (session && session.stream.writable) { try { const out = session.indexer.push(Buffer.from(chunk)); if (out.length) session.stream.write(out); session.chunks = (session.chunks || 0) + 1; session.bytes = (session.bytes || 0) + chunk.length; } catch (_) {} } });
ipcMain.on('rec:first-frame', (_e, buf) => { if (session) session.firstFrame = Buffer.from(buf); });
ipcMain.on('rec:done', (_e, info) => finalize(info || {}));
ipcMain.on('rec:error', (_e, msg) => { events.emit('error', msg); if (session) { session.canceled = true; finalize({ duration: 0 }); } });
ipcMain.on('rec:trace', (_e, msg) => { trace.push(msg); if (trace.length > 30) trace.shift(); });
ipcMain.on('rec:level', (_e, levels) => {
  if (!session) return;
  session.levels = levels;
  const w = getControlsWindow();
  if (w) w.webContents.send('rec:level', levels);
});
ipcMain.on('rec:controls-stop', () => stopRecording());
ipcMain.on('rec:controls-pause', () => (session && session.paused ? resumeRecording() : pauseRecording()));
ipcMain.on('rec:controls-cancel', () => cancelRecording());

function getControlsWindow() { return session && session.controlsWin && !session.controlsWin.isDestroyed() ? session.controlsWin : null; }
function getBorderWindow() { return session && session.borderWin && !session.borderWin.isDestroyed() ? session.borderWin : null; }
function getRecorderWindow() { return session && session.recWin && !session.recWin.isDestroyed() ? session.recWin : null; }

module.exports = { init, events, startRecording, stopRecording, pauseRecording, resumeRecording, cancelRecording, isRecording, status, getControlsWindow, getBorderWindow, getRecorderWindow };
