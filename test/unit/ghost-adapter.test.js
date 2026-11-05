'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');
const { test, assert, eq, near } = require('../tiny');
const { createAdapter } = require('../ghost/adapter');
const { toneAmplitudes, findMediaTool } = require('../ghost/media');

test('ghost adapter: readiness waits for main UI after the HTTP listener', async () => {
  const events = [];
  let polls = 0;
  const app = createAdapter({ impl: 'rust',
    waitReady: async () => events.push('http ready'),
    sleep: async () => events.push('wait'),
    post: async (route, body) => {
      eq([route, body], ['ui-state', { target: 'main' }]);
      events.push('main state'); return { ready: ++polls > 1 };
    },
  });
  await app.waitReady();
  eq(events, ['http ready', 'main state', 'wait', 'main state']);
});

test('ghost adapter: Rust operations preserve route bodies and results', async () => {
  const calls = [];
  const value = { marker: 'actual result' };
  const app = createAdapter({ post: async (route, body) => { calls.push([route, body]); return value; } });
  const patch = { recording: { microphone: false } };
  const region = { displayId: 7, rectDip: { x: 1, y: 2, w: 3, h: 4 }, rectPhys: { x: 2, y: 4, w: 6, h: 8 } };
  eq(await app.settingsSet(patch), value);
  eq(await app.historyList(3), value);
  eq(await app.exportGif('fixture-id'), value);
  eq(await app.cancelOverlay(), value);
  eq(await app.recordStart(region), value);
  eq(await app.recordStart(), value);
  eq(await app.capturePage('overlay', 'fake.png', 2), value);
  eq(calls, [
    ['settings-set', { patch }], ['history-list', { limit: 3 }],
    ['export-gif', { id: 'fixture-id' }], ['overlay-cancel', undefined],
    ['record-start', region], ['record-start', {}],
    ['capture-page', { target: 'overlay', index: 2, path: 'fake.png' }],
  ]);
});

test('ghost adapter: Rust UI actions and snapshots stay semantic and indexed', async () => {
  const calls = [];
  const snapshot = { original: 'hello', translated: '', loading: true };
  const app = createAdapter({ post: async (route, body) => { calls.push([route, body]); return snapshot; } });
  eq(await app.uiState('tp'), snapshot);
  const action = { type: 'select', rect: { x: 20, y: 30, w: 320, h: 180 } };
  eq(await app.uiAction('overlay', action, 1), snapshot);
  eq(await app.pointerDownDip({ x: 20619, y: 354 }), snapshot);
  eq(calls, [
    ['ui-state', { target: 'tp' }],
    ['ui-action', { target: 'overlay', index: 1, action }],
    ['ui-action', { target: 'bubble', action: { type: 'pointer-down', point: { x: 20619, y: 354 } } }],
  ]);
});

test('ghost adapter: Rust diagnostics do not invent successful finalization', async () => {
  const diagnostics = { audioCodec: null, indexed: false, screenVideoTracks: null, screenAudioTracks: null };
  const app = createAdapter({ post: async (route, body) => {
    eq([route, body], ['ui-state', { target: 'rec-controls' }]);
    return { meters: [], diagnostics };
  } });
  eq(await app.recordingDiagnostics(), diagnostics);
});

test('ghost adapter: accepted format and fallback differences stay explicit', () => {
  const rust = createAdapter({ post: async () => null });
  eq([rust.videoExtension('auto'), rust.videoExtension('mp4')], ['mp4', 'mp4']);
  assert(rust.isCaptureFallback('Captured window via windows-graphics-capture'));
  assert(!rust.isCaptureFallback('Captured window via printwindow'));
  assert(!rust.isCaptureFallback('Captured window without a method'));
});

test('ghost media: first-channel PCM preserves tone amplitudes and observation interval', () => {
  const rate = 48000;
  for (const [system, microphone] of [[0.11, 0], [0, 0.09], [0.11, 0.09]]) {
    const pcm = Buffer.alloc(rate * 2 * 4);
    for (let i = 0; i < rate * 2; i++) {
      const inside = i >= rate * 0.2 && i < rate * 1.2;
      const sample = inside
        ? system * Math.sin(2 * Math.PI * 440 * i / rate) + microphone * Math.sin(2 * Math.PI * 880 * i / rate)
        : 0.7 * Math.sin(2 * Math.PI * 440 * i / rate);
      pcm.writeFloatLE(sample, i * 4);
    }
    const decoded = toneAmplitudes(pcm, rate);
    near(decoded.system, system, 0.0001, '440 Hz');
    near(decoded.microphone, microphone, 0.0001, '880 Hz');
    eq(decoded.duration, 2);
    assert(system ? decoded.system > 0.035 : decoded.system < 0.015);
    assert(microphone ? decoded.microphone > 0.035 : decoded.microphone < 0.015);
  }
});

test('ghost media: FFmpeg lookup prefers config, then PATH, then nested WinGet package', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-media-tools-'));
  const exe = process.platform === 'win32' ? 'ffmpeg.exe' : 'ffmpeg';
  const probe = process.platform === 'win32' ? 'ffprobe.exe' : 'ffprobe';
  try {
    const config = path.join(dir, 'configured');
    const onPath = path.join(dir, 'on-path');
    const winget = path.join(dir, 'Microsoft', 'WinGet', 'Packages', 'Gyan.FFmpeg_Test', 'nested', 'build', 'bin');
    for (const d of [config, onPath, winget]) {
      fs.mkdirSync(d, { recursive: true }); fs.writeFileSync(path.join(d, exe), 'fixture');
      fs.writeFileSync(path.join(d, probe), 'fixture');
    }
    const env = { FFMPEG_PATH: path.join(config, exe), PATH: onPath, LOCALAPPDATA: dir };
    eq(findMediaTool('ffmpeg', env), path.join(config, exe));
    eq(findMediaTool('ffprobe', env), path.join(config, probe));
    env.FFMPEG_PATH = config;
    eq(findMediaTool('ffmpeg', env), path.join(config, exe));
    env.FFMPEG_PATH = path.join(dir, 'missing', exe);
    eq(findMediaTool('ffmpeg', env), path.join(onPath, exe));
    delete env.FFMPEG_PATH;
    eq(findMediaTool('ffmpeg', env), path.join(onPath, exe));
    env.PATH = '';
    eq(findMediaTool('ffmpeg', env), path.join(winget, exe));
    eq(findMediaTool('ffprobe', env), path.join(winget, probe));
    env.LOCALAPPDATA = path.join(dir, 'missing');
    let error;
    try { findMediaTool('ffmpeg', env); } catch (e) { error = e; }
    assert(error && /ffmpeg is required.*FFMPEG_PATH.*WinGet/.test(error.message), 'clear missing-tool failure');
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
