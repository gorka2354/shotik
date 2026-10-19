'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');
const { test, assert, eq } = require('../tiny');
const { makeApp } = require('./harness');
const { decodeAudio } = require('./media');

let port = 7560;
async function waitFor(app, check) {
  for (let i = 0; i < 60; i++) {
    const result = await check();
    if (result) return result;
    await app.sleep(100);
  }
  throw new Error('Recorder did not reach expected state: ' + JSON.stringify(await app.post('record-status')));
}

for (const config of [
  { microphone: false, systemAudio: false, format: 'auto' },
  { microphone: true, systemAudio: false, format: 'auto' },
  { microphone: false, systemAudio: true, format: 'auto' },
  { microphone: true, systemAudio: true, format: 'auto' },
  { microphone: true, systemAudio: true, format: 'mp4' },
]) {
  test('recording file: ' + JSON.stringify(config), async () => {
    const app = makeApp({ port: port++ });
    const saveDir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-rec-audio-'));
    try {
      await app.waitReady();
      await app.post('exec-js', { target: 'main', code: `window.shotik.setSettings(${JSON.stringify({ saveDir, showToast: false, recording: { ...config, countdown: false } })})` });
      await app.post('record-start');
      const status = await waitFor(app, async () => {
        const s = await app.post('record-status'); return s.audio && s;
      });
      eq(status.audio, { system: config.systemAudio, microphone: config.microphone, tracks: config.microphone || config.systemAudio ? 1 : 0 });
      await app.sleep(1700);
      await app.post('record-stop');
      const finished = await waitFor(app, async () => {
        const s = await app.post('state'); return s.lastProcessed && s.lastProcessed.action === 'record-done' && s.lastProcessed;
      });
      assert(fs.statSync(finished.file).size > 1000, 'nonempty video saved');
      if (config.microphone || config.systemAudio) {
        const audio = await decodeAudio(app, finished.file);
        assert(audio.duration > 1, 'decodable audio duration: ' + JSON.stringify(audio));
        for (const [signal, enabled] of [['system', config.systemAudio], ['microphone', config.microphone]]) {
          assert(enabled ? audio[signal] > 0.035 : audio[signal] < 0.015, signal + ' signal in encoded file: ' + JSON.stringify(audio));
        }
      }
      eq((await app.post('record-status')).recording, false);
    } finally { await app.stop(); }
  });
}

test('microphone settings: toggle, selected device and unavailable device survive reload', async () => {
  const app = makeApp({ port: port++ });
  try {
    await app.waitReady();
    await app.post('exec-js', { target: 'main', code: `window.shotik.setSettings({language:'en',recording:{microphone:false,microphoneDeviceId:'default'}}).then(()=>location.reload())` });
    await waitFor(app, () => app.post('exec-js', { target: 'main', code: '!!state && !!DICT.recMicrophone' }));
    await app.post('exec-js', { target: 'main', code: `document.querySelector('#setMicrophone').click(); 'ok'` });
    await waitFor(app, () => app.post('exec-js', { target: 'main', code: `state.settings.recording.microphone && !document.querySelector('#microphoneDeviceRow').hidden` }));
    await app.post('exec-js', { target: 'main', code: `applySettings({recording:{microphoneDeviceId:'unplugged-device'}})` });
    await waitFor(app, () => app.post('exec-js', { target: 'main', code: `document.querySelector('#setMicrophoneDevice').value === 'unplugged-device'` }));
    const label = await app.post('exec-js', { target: 'main', code: `document.querySelector('#setMicrophoneDevice').selectedOptions[0].textContent` });
    eq(label, 'Selected microphone is unavailable');
    await app.post('exec-js', { target: 'main', code: `location.reload(); 'ok'` });
    await waitFor(app, () => app.post('exec-js', { target: 'main', code: `!!state && document.querySelector('#setMicrophoneDevice').value === 'unplugged-device'` }));
    eq(await app.post('exec-js', { target: 'main', code: `document.querySelector('#setMicrophone').checked` }), true);
  } finally { await app.stop(); }
});
