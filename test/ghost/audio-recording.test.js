'use strict';
// Sound-only recording (calls): synthetic 440 Hz "system audio" and 880 Hz
// "microphone" — no real devices, nothing on screen. Checks the file the way a
// user (and the call-transcription pipeline) would: decoded tones, length and
// seeking in Chromium and Windows Media Player, the history card, the level
// meters in the controls bar.
const fs = require('fs');
const os = require('os');
const path = require('path');
const { test, assert, eq, near } = require('../tiny');
const { makeApp } = require('./harness');
const { decodeAudio, mediaFoundation } = require('./media');

let port = 7620;
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
  await app.post('exec-js', { target: 'main', code: `window.shotik.setSettings(${JSON.stringify({ saveDir, showToast: true, audioRecording })})` });
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
  const app = makeApp({ port: port++ });
  let saveDir;
  try {
    await app.waitReady();
    const rec = await recordAudio(app, { systemAudio: true, microphone: true });
    saveDir = rec.saveDir;
    assert(rec.file.endsWith('.m4a'), 'saved as .m4a: ' + rec.file);
    eq(rec.status.audio, { system: true, microphone: true, tracks: 1 });
    const trace = (await app.post('record-status')).trace;
    assert(trace.some((l) => /recorder\.start audio\/mp4;codecs=mp4a\.40\.2/.test(l)), 'AAC encoder: ' + JSON.stringify(trace));
    assert(trace.some((l) => /^index ok/.test(l)), 'duration indexed: ' + JSON.stringify(trace));
    assert(fs.statSync(rec.file).size < 200 * 1024, 'a few seconds of audio stay small: ' + fs.statSync(rec.file).size);

    const audio = await decodeAudio(app, rec.file);
    assert(audio.system > 0.035 && audio.microphone > 0.035, 'both inputs in the file: ' + JSON.stringify(audio));

    const chromium = await app.post('media-probe', { file: rec.file });
    assert(typeof chromium.duration === 'number', 'Chromium sees a duration: ' + JSON.stringify(chromium));
    near(chromium.duration, RECORD_MS / 1000, 1.5, 'duration');
    assert(chromium.seeked, 'Chromium seeks: ' + JSON.stringify(chromium));
    const mf = mediaFoundation(rec.file, 2);
    assert(mf.canSeek === true, 'Media Foundation can seek: ' + JSON.stringify(mf));
    near(mf.durationSec, chromium.duration, 0.1, 'Media Foundation duration');
    near(mf.positionAfterSeek, 2, 0.3, 'Media Foundation position after seek');

    const [entry] = await app.post('exec-js', { target: 'main', code: 'window.shotik.historyList()' });
    eq([entry.kind, entry.file], ['audio', rec.file], 'history card is an audio entry');
    const toast = await app.post('last-toast');
    assert(toast && toast.kind === 'audio' && /^(Audio|Звук) · /.test(toast.body), 'audio toast: ' + JSON.stringify(toast));
  } finally {
    await app.stop();
    if (saveDir) fs.rmSync(saveDir, { recursive: true, force: true });
  }
});

test('audio recording: microphone only never asks for the screen', async () => {
  const app = makeApp({ port: port++ });
  let saveDir;
  try {
    await app.waitReady();
    const rec = await recordAudio(app, { systemAudio: false, microphone: true });
    saveDir = rec.saveDir;
    eq(rec.status.audio, { system: false, microphone: true, tracks: 1 });
    const trace = (await app.post('record-status')).trace;
    assert(trace.some((l) => /vtracks=0 atracks=0/.test(l)), 'no screen stream: ' + JSON.stringify(trace));
    const audio = await decodeAudio(app, rec.file);
    assert(audio.microphone > 0.035 && audio.system < 0.015, 'microphone only: ' + JSON.stringify(audio));
  } finally {
    await app.stop();
    if (saveDir) fs.rmSync(saveDir, { recursive: true, force: true });
  }
});

test('audio recording: with both inputs off it explains instead of recording silence', async () => {
  const app = makeApp({ port: port++ });
  try {
    await app.waitReady();
    await app.post('exec-js', { target: 'main', code: `window.shotik.setSettings(${JSON.stringify({ showToast: true, audioRecording: { systemAudio: false, microphone: false } })})` });
    await app.post('trigger', { mode: 'audio' });
    eq((await app.post('record-status')).recording, false);
    const toast = await app.post('last-toast');
    assert(toast && /Settings → Audio recording|Настройки → Запись звука/.test(toast.body), 'explains what to turn on: ' + JSON.stringify(toast));
  } finally { await app.stop(); }
});

test('audio recording: controls bar shows a live level meter per input', async () => {
  const app = makeApp({ port: port++, env: { SHOTIK_FORCE_CONTROLS: '1' } });
  let saveDir;
  try {
    await app.waitReady();
    const rec = await recordAudio(app, { systemAudio: true, microphone: true }, {
      duringRecording: async () => {
        const meters = await until(app, async () => {
          // rendered size, not the style value: an inline .fill once kept width:83% and drew nothing.
          // The off-screen window counts as occluded, which freezes CSS transitions — switch them off.
          const m = await app.post('exec-js', { target: 'rec-controls', code: `document.head.insertAdjacentHTML('beforeend', '<style>.fill{transition:none!important}</style>'); [...document.querySelectorAll('.meter')].map((el) => ({ input: el.dataset.input, track: el.querySelector('.track').getBoundingClientRect().width, fill: el.querySelector('.fill').getBoundingClientRect().width }))` }).catch(() => null);
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
