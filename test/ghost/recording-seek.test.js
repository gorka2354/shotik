'use strict';
// Regression: MediaRecorder WebM had no Duration/Cues (fragmented MP4: no mvhd
// duration), so the Windows player showed no length and could only play/pause —
// the timeline did nothing.
// Records a real (synthetic-source) video and checks it in both engines a
// user actually opens it with: Chromium and Media Foundation (Windows Media Player).
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');
const { test, assert, near } = require('../tiny');
const { makeApp } = require('./harness');

let port = 7600;
const RECORD_MS = 4500; // > one keyframe interval (100 frames @30fps) → two clusters

async function recordFile(app, format) {
  const saveDir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-rec-seek-'));
  await app.post('exec-js', { target: 'main', code: `window.shotik.setSettings(${JSON.stringify({ saveDir, showToast: false, recording: { format, microphone: false, systemAudio: true, countdown: false } })})` });
  await app.post('record-start');
  await app.sleep(RECORD_MS);
  await app.post('record-stop');
  for (let i = 0; i < 80; i++) {
    const s = await app.post('state');
    if (s.lastProcessed && s.lastProcessed.action === 'record-done') return { file: s.lastProcessed.file, saveDir };
    await app.sleep(100);
  }
  throw new Error('recording did not finish: ' + JSON.stringify(await app.post('record-status')));
}

function mediaFoundation(file, seekTo) {
  const out = execFileSync('powershell.exe', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File',
    path.join(__dirname, 'mf-probe.ps1'), '-Path', file, '-SeekTo', String(seekTo)], { encoding: 'utf8', timeout: 60000 });
  return JSON.parse(out.trim().split(/\r?\n/).pop());
}

for (const [format, ext] of [['auto', 'webm'], ['mp4', 'mp4']]) {
test(`recording seek: ${ext} has a duration and seeks in Chromium and Windows Media Player`, async () => {
  const app = makeApp({ port: port++ });
  let saveDir;
  try {
    await app.waitReady();
    const rec = await recordFile(app, format);
    saveDir = rec.saveDir;
    assert(rec.file.endsWith('.' + ext), `${format} format saves .${ext}: ${rec.file}`);
    const trace = (await app.post('record-status')).trace;
    assert(trace.some((l) => /^index ok/.test(l)), 'indexer ran: ' + JSON.stringify(trace));

    const chromium = await app.post('media-probe', { file: rec.file });
    assert(typeof chromium.duration === 'number', 'Chromium sees a finite duration: ' + JSON.stringify(chromium));
    near(chromium.duration, RECORD_MS / 1000, 1.5, 'duration');
    assert(chromium.seeked, 'Chromium seek completes: ' + JSON.stringify(chromium));
    near(chromium.position, chromium.seekTo, 0.2, 'Chromium position after seek');

    const seekTo = Math.round(chromium.duration * 0.75 * 10) / 10;
    const mf = mediaFoundation(rec.file, seekTo);
    assert(mf.canSeek === true, 'Media Foundation can seek: ' + JSON.stringify(mf));
    near(mf.durationSec, chromium.duration, 0.1, 'Media Foundation duration');
    near(mf.positionAfterSeek, seekTo, 0.3, 'Media Foundation position after seek');
  } finally {
    await app.stop();
    if (saveDir) fs.rmSync(saveDir, { recursive: true, force: true });
  }
});
}
