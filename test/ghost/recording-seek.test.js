'use strict';
// Regression: MediaRecorder WebM had no Duration/Cues (fragmented MP4: no mvhd
// duration), so the Windows player showed no length and could only play/pause —
// the timeline did nothing.
// Records a real (synthetic-source) video and checks it in both engines a
// user actually opens it with: the file player and Media Foundation (Windows Media Player).
const fs = require('fs');
const os = require('os');
const path = require('path');
const { test, assert, near } = require('../tiny');
const { makeApp } = require('./adapter');
const { mediaFoundation } = require('./media');

const RECORD_MS = 4500; // > one keyframe interval (100 frames @30fps) → two clusters

async function recordFile(app, format) {
  const saveDir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-rec-seek-'));
  await app.settingsSet({ saveDir, showToast: false, recording: { format, microphone: false, systemAudio: true, countdown: false } });
  await app.recordStart();
  await app.sleep(RECORD_MS);
  await app.post('record-stop');
  for (let i = 0; i < 80; i++) {
    const s = await app.post('state');
    if (s.lastProcessed && s.lastProcessed.action === 'record-done') return { file: s.lastProcessed.file, saveDir };
    await app.sleep(100);
  }
  throw new Error('recording did not finish: ' + JSON.stringify(await app.post('record-status')));
}

for (const format of ['auto', 'mp4']) {
test(`recording seek: ${format} has a duration and seeks in the file player and Windows Media Player`, async () => {
  const app = makeApp();
  let saveDir;
  try {
    await app.waitReady();
    const rec = await recordFile(app, format);
    saveDir = rec.saveDir;
    const ext = app.videoExtension(format);
    assert(rec.file.endsWith('.' + ext), `${format} format saves .${ext}: ${rec.file}`);
    const diagnostics = await app.recordingDiagnostics();
    assert(diagnostics.indexed, 'file index finalized: ' + JSON.stringify(diagnostics));

    const player = await app.probeMedia(rec.file);
    assert(typeof player.duration === 'number', 'player sees a finite duration: ' + JSON.stringify(player));
    near(player.duration, RECORD_MS / 1000, 1.5, 'duration');
    assert(player.seeked, 'player seek completes: ' + JSON.stringify(player));
    near(player.position, player.seekTo, 0.2, 'player position after seek');

    const seekTo = Math.round(player.duration * 0.75 * 10) / 10;
    const mf = mediaFoundation(rec.file, seekTo);
    assert(mf.canSeek === true, 'Media Foundation can seek: ' + JSON.stringify(mf));
    near(mf.durationSec, player.duration, 0.1, 'Media Foundation duration');
    near(mf.positionAfterSeek, seekTo, 0.3, 'Media Foundation position after seek');
  } finally {
    await app.stop();
    if (saveDir) fs.rmSync(saveDir, { recursive: true, force: true });
  }
});
}
