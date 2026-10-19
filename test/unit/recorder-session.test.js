'use strict';
// The main-process recording session with a stub `electron`: what a second
// start or a stop does while the 2-second countdown is still running.
const fs = require('fs');
const os = require('os');
const path = require('path');
const { test, assert, eq } = require('../tiny');

const windows = [];
class FakeWindow {
  constructor(opts) {
    this.opts = opts; this.destroyed = false; this.loaded = null;
    this.webContents = { once() {}, send() {}, executeJavaScript: async () => {} };
    windows.push(this);
  }
  removeMenu() {} setIgnoreMouseEvents() {} setAlwaysOnTop() {}
  loadFile(file, o) { this.loaded = { file: path.basename(file), query: o && o.query }; }
  isDestroyed() { return this.destroyed; }
  destroy() { this.destroyed = true; }
}
const display = { id: 1, scaleFactor: 1, size: { width: 1920, height: 1080 }, bounds: { x: 0, y: 0, width: 1920, height: 1080 }, workArea: { x: 0, y: 0, width: 1920, height: 1040 } };
const stub = {
  BrowserWindow: FakeWindow,
  desktopCapturer: { getSources: async () => [{ id: 'screen:1:0', display_id: '1' }] },
  screen: { getAllDisplays: () => [display], getPrimaryDisplay: () => display },
  app: { getPath: () => os.tmpdir() },
  ipcMain: { on() {} },
  session: { defaultSession: { setDisplayMediaRequestHandler() {} } },
};
require.cache[require.resolve('electron')] = { id: 'electron', filename: 'electron', loaded: true, exports: stub };
const recorder = require('../../src/main/recorder');

const saveDir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-session-'));
process.on('exit', () => fs.rmSync(saveDir, { recursive: true, force: true }));
let settings;
recorder.init({ settings: { get: () => settings }, history: { addVideo: (e) => e } });
const video = { displayId: 1, rectPhys: { x: 0, y: 0, w: 320, h: 180 }, rectDip: { x: 0, y: 0, w: 320, h: 180 } };
const recorderWindows = () => windows.filter((w) => w.loaded && w.loaded.file === 'recorder.html');

function reset() {
  windows.length = 0;
  settings = { saveDir, recording: { countdown: true, systemAudio: false, microphone: false }, audioRecording: { systemAudio: true, microphone: true } };
}

async function cleanup() {
  recorder.cancelRecording();
  await new Promise((r) => setTimeout(r, 800)); // cancel → finalize → close stream → unlink
}

test('recording session: audio start during the video countdown is refused, one session wins', async () => {
  reset();
  const first = recorder.startRecording(video);
  assert(recorder.isRecording(), 'counts as recording while the countdown runs');
  eq(await recorder.startRecording({ audioOnly: true }), null, 'second start refused');
  const status = await first;
  eq([status.recording, status.audioOnly], [true, false], 'the video session is the one running');
  eq(recorderWindows().length, 1, 'exactly one recorder window');
  await cleanup();
});

test('recording session: stop during the countdown calls the start off', async () => {
  reset();
  const first = recorder.startRecording(video);
  recorder.stopRecording();
  eq(await first, null);
  eq(recorder.isRecording(), false);
  eq(recorderWindows().length, 0, 'no capture ever started');
  eq(windows.every((w) => w.destroyed || w.loaded.file === 'countdown.html'), true);
});

test('recording session: audio mode skips the countdown and builds no region border', async () => {
  reset();
  const status = await recorder.startRecording({ audioOnly: true });
  eq([status.recording, status.audioOnly], [true, true]);
  const files = windows.map((w) => w.loaded && w.loaded.file);
  assert(!files.includes('countdown.html') && !files.includes('border.html'), 'no countdown/border: ' + files);
  const rec = recorderWindows()[0];
  eq([rec.loaded.query.audioOnly, rec.loaded.query.audio, rec.loaded.query.microphone], ['1', '1', '1'], 'inputs come from audioRecording');
  const controls = windows.find((w) => w.loaded && w.loaded.file === 'controls.html');
  eq(controls.loaded.query.meters, 'system,microphone');
  assert(controls.opts.y + controls.opts.height <= display.workArea.height, 'controls sit above the taskbar');
  await cleanup();
});
