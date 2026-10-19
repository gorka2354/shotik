'use strict';
// MCP access to recordings: Claude finds the last call (.m4a) or screen video
// by path, and the screenshot tools no longer trip over recordings that share
// the same history (get_last_screenshot used to read a .m4a as an image).
const fs = require('fs');
const os = require('os');
const path = require('path');
const { test, assert, eq, near } = require('../tiny');
const { makeApp } = require('./harness');
const { mcpCall, imgOf, textOf } = require('./mcp-client');

let port = 7640;

async function until(app, check, what) {
  for (let i = 0; i < 80; i++) {
    const r = await check();
    if (r) return r;
    await app.sleep(100);
  }
  throw new Error(what);
}
const doneFile = (app) => until(app, async () => {
  const s = await app.post('state');
  return s.lastProcessed && s.lastProcessed.action === 'record-done' && s.lastProcessed.file;
}, 'recording finished');
const json = (res) => JSON.parse(textOf(res));

test('mcp: recordings are listed by kind, the last call is one call away, screenshots stay images', async () => {
  const app = makeApp({ port });
  const saveDir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-mcp-rec-'));
  try {
    await app.waitReady();
    await app.post('exec-js', { target: 'main', code: `window.shotik.setSettings(${JSON.stringify({ saveDir, autoSave: true, showToast: false, recording: { countdown: false, systemAudio: true, microphone: false }, audioRecording: { systemAudio: true, microphone: true } })})` });

    // history, oldest → newest: screenshot, screen video, audio call
    const shot = await app.post('trigger', { mode: 'full' });
    await app.post('record-start'); await app.sleep(2200); await app.post('record-stop');
    const videoFile = await doneFile(app);
    await app.post('trigger', { mode: 'audio' });
    await until(app, async () => (await app.post('record-status')).audio, 'audio recorder ready');

    // while the call is still being recorded, Claude is told so
    const during = await mcpCall(port, 'get_last_recording', { kind: 'audio' });
    assert(/in progress right now/.test(during.content.map((c) => c.text).join('\n')), 'in-progress note: ' + JSON.stringify(during));

    await app.sleep(2500); await app.post('record-stop');
    const audioFile = await until(app, async () => { const f = await doneFile(app); return f !== videoFile && f; }, 'audio finished');

    // the regression: the newest history entry is a recording, the tool must still return the screenshot
    const lastShot = await mcpCall(port, 'get_last_screenshot');
    assert(!lastShot.isError && imgOf(lastShot), 'an image, not an error: ' + textOf(lastShot));
    assert(textOf(lastShot).startsWith(shot.file), 'the screenshot taken above: ' + textOf(lastShot));
    const shots = json(await mcpCall(port, 'list_screenshots', { limit: 50 }));
    assert(shots.every((s) => !/\.(m4a|webm|mp4)$/.test(s.file)), 'no recordings among screenshots');

    const call = json(await mcpCall(port, 'get_last_recording', { kind: 'audio' }));
    eq([call.file, call.kind], [audioFile, 'audio']);
    assert(call.file.endsWith('.m4a') && fs.existsSync(call.file), 'existing .m4a path');
    near(call.duration_sec, 2.5, 1.5, 'duration');
    assert(typeof call.size_mb === 'number' && !('width' in call), 'size, no frame size for audio: ' + JSON.stringify(call));

    const any = json(await mcpCall(port, 'get_last_recording'));
    eq(any.file, audioFile, 'newest of any kind');
    const vid = json(await mcpCall(port, 'get_last_recording', { kind: 'video' }));
    eq([vid.file, vid.kind, vid.width, vid.height], [videoFile, 'video', 320, 180]);

    const recent = json(await mcpCall(port, 'list_recordings', { limit: 2 }));
    eq(recent.map((r) => r.file), [audioFile, videoFile], 'newest first, both kinds');
    const audioOnly = json(await mcpCall(port, 'list_recordings', { kind: 'audio', limit: 50 }));
    assert(audioOnly.length >= 1 && audioOnly.every((r) => r.kind === 'audio'), 'kind filter');

    const bad = await mcpCall(port, 'list_recordings', { kind: 'mp3' });
    assert(bad.isError && /"kind" must be "audio" or "video"/.test(textOf(bad)), textOf(bad));
    const junkLimit = json(await mcpCall(port, 'list_recordings', { limit: -3 }));
    eq(junkLimit.length, Math.min(10, json(await mcpCall(port, 'list_recordings', { limit: 50 })).length), 'a negative limit means the default, not 1');

    // empty history: a list is still JSON ([]), the "last" tool explains
    await app.post('exec-js', { target: 'main', code: 'window.shotik.historyClear({ deleteFiles: false })' });
    eq(json(await mcpCall(port, 'list_recordings', { kind: 'audio' })), []);
    eq(textOf(await mcpCall(port, 'get_last_recording', { kind: 'audio' })), 'No audio recordings in history yet.');
  } finally {
    await app.stop();
    fs.rmSync(saveDir, { recursive: true, force: true });
  }
});
