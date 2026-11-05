'use strict';
// Sound-only recording (calls): synthetic 440 Hz "system audio" and 880 Hz
// "microphone" — no real devices, nothing on screen. Checks the file the way a
// user (and the call-transcription pipeline) would: decoded tones, length and
// seeking in the file player and Windows Media Player, the history card, the level
// meters in the controls bar.
const fs = require('fs');
const os = require('os');
const path = require('path');
const { test, assert, eq, near } = require('../tiny');
const { makeApp } = require('./adapter');
const { decodeAudio, mediaFoundation } = require('./media');

const RECORD_MS = 4000;

async function until(app, check, what) {
  for (let i = 0; i < 80; i++) {
    const r = await check();
    if (r) return r;
    await app.sleep(100);
  }
  throw new Error(what + ': ' + JSON.stringify(await app.post('record-status')));
}

async function recordAudio(app, audioRecording, { duringRecording } = {}) {
  const saveDir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-rec-audio-only-'));
  await app.settingsSet({ saveDir, showToast: true, audioRecording });
  const started = await app.post('trigger', { mode: 'audio' });
  eq(started.recording, true, 'audio recording started');
  eq(started.audioOnly, true);
  const status = await until(app, async () => { const s = await app.post('record-status'); return s.audio && s.levels && s; }, 'recorder ready with levels');
  if (duringRecording) await duringRecording(status);
  await app.sleep(RECORD_MS);
  await app.post('record-stop');
  const done = await until(app, async () => { const s = await app.post('state'); return s.lastProcessed && s.lastProcessed.action === 'record-done' && s.lastProcessed; }, 'recording finished');
  return { file: done.file, saveDir, status };
}

test('audio recording: system audio + microphone → seekable .m4a with both voices', async () => {
  const app = makeApp();
  let saveDir;
  try {
    await app.waitReady();
    const rec = await recordAudio(app, { systemAudio: true, microphone: true });
    saveDir = rec.saveDir;
    assert(rec.file.endsWith('.m4a'), 'saved as .m4a: ' + rec.file);
    eq(rec.status.audio, { system: true, microphone: true, tracks: 1 });
    const diagnostics = await app.recordingDiagnostics();
    eq(diagnostics.audioCodec, 'aac', 'AAC encoder: ' + JSON.stringify(diagnostics));
    assert(diagnostics.indexed, 'duration indexed: ' + JSON.stringify(diagnostics));
    assert(fs.statSync(rec.file).size < 200 * 1024, 'a few seconds of audio stay small: ' + fs.statSync(rec.file).size);

    const audio = await decodeAudio(rec.file);
    assert(audio.system > 0.035 && audio.microphone > 0.035, 'both inputs in the file: ' + JSON.stringify(audio));

    const player = await app.probeMedia(rec.file);
    assert(typeof player.duration === 'number', 'player sees a duration: ' + JSON.stringify(player));
    near(player.duration, RECORD_MS / 1000, 1.5, 'duration');
    const mf = mediaFoundation(rec.file, 2);
    // Windows' player opens a file with sound only when there is a device to play it on, and a build server
    // has none. ffprobe and the decoded tones above have checked the file by then.
    if (!(process.env.CI && mf.state === 'None')) {
      assert(player.seeked, 'player seeks: ' + JSON.stringify(player));
      assert(mf.canSeek === true, 'Media Foundation can seek: ' + JSON.stringify(mf));
      near(mf.durationSec, player.duration, 0.1, 'Media Foundation duration');
      near(mf.positionAfterSeek, 2, 0.3, 'Media Foundation position after seek');
    }

    const [entry] = await app.historyList();
    eq([entry.kind, entry.file], ['audio', rec.file], 'history card is an audio entry');
    const toast = await app.post('last-toast');
    assert(toast && toast.kind === 'audio' && /^(Audio|Звук) · /.test(toast.body), 'audio toast: ' + JSON.stringify(toast));
  } finally {
    await app.stop();
    if (saveDir) fs.rmSync(saveDir, { recursive: true, force: true });
  }
});

test('audio recording: microphone only never asks for the screen', async () => {
  const app = makeApp();
  let saveDir;
  try {
    await app.waitReady();
    const rec = await recordAudio(app, { systemAudio: false, microphone: true });
    saveDir = rec.saveDir;
    eq(rec.status.audio, { system: false, microphone: true, tracks: 1 });
    const diagnostics = await app.recordingDiagnostics();
    assert(diagnostics.screenVideoTracks === 0 && diagnostics.screenAudioTracks === 0, 'no screen stream: ' + JSON.stringify(diagnostics));
    const audio = await decodeAudio(rec.file);
    assert(audio.microphone > 0.035 && audio.system < 0.015, 'microphone only: ' + JSON.stringify(audio));
  } finally {
    await app.stop();
    if (saveDir) fs.rmSync(saveDir, { recursive: true, force: true });
  }
});

test('audio recording: with both inputs off it explains instead of recording silence', async () => {
  const app = makeApp();
  try {
    await app.waitReady();
    await app.settingsSet({ showToast: true, audioRecording: { systemAudio: false, microphone: false } });
    await app.post('trigger', { mode: 'audio' });
    eq((await app.post('record-status')).recording, false);
    const toast = await app.post('last-toast');
    assert(toast && /Settings → Audio recording|Настройки → Запись звука/.test(toast.body), 'explains what to turn on: ' + JSON.stringify(toast));
  } finally { await app.stop(); }
});

test('audio recording: controls bar shows a live level meter per input', async () => {
  const app = makeApp({ env: { SHOTIK_FORCE_CONTROLS: '1' } });
  let saveDir;
  try {
    await app.waitReady();
    const rec = await recordAudio(app, { systemAudio: true, microphone: true }, {
      duringRecording: async () => {
        const meters = await until(app, async () => {
          // rendered size, not the style value: an inline .fill once kept width:83% and drew nothing.
          // The off-screen window counts as occluded, which freezes CSS transitions — switch them off.
          const m = await app.uiState('rec-controls').then((s) => s.meters).catch(() => null);
          return Array.isArray(m) && m.length === 2 && m.every((x) => x.fill > 0.2 * x.track) && m;
        }, 'meters filled');
        eq(meters.map((m) => m.input), ['system', 'microphone']);
      },
    });
    saveDir = rec.saveDir;
  } finally {
    await app.stop();
    if (saveDir) fs.rmSync(saveDir, { recursive: true, force: true });
  }
});
