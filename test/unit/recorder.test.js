'use strict';
const fs = require('fs');
const path = require('path');
const vm = require('vm');
const { test, assert, eq } = require('../tiny');

const source = fs.readFileSync(path.join(__dirname, '../../src/recorder/recorder.js'), 'utf8');
function stream(kind = 'audio') {
  const track = { kind, readyState: 'live', stop() { this.readyState = 'ended'; } };
  return { track, getTracks: () => [track], getAudioTracks: () => kind === 'audio' ? [track] : [],
    getVideoTracks: () => kind === 'video' ? [track] : [] };
}
function fixture(getUserMedia, device = 'default') {
  const display = stream('video');
  const errors = [], done = [];
  let control;
  const context = vm.createContext({
    URLSearchParams, location: { search: '?test=0&microphone=1&microphoneDeviceId=' + encodeURIComponent(device) },
    navigator: { mediaDevices: { getUserMedia, getDisplayMedia: async () => display } },
    window: { rec: { trace() {}, error: (e) => errors.push(e), done: (e) => done.push(e), onControl: (cb) => { control = cb; } } },
    clearInterval, clearTimeout, cancelAnimationFrame() {}, performance,
  });
  vm.runInContext(source, context);
  return { context, display, errors, done, stop: () => control('stop') };
}

test('recorder: selected microphone uses exact device, never a silent fallback', async () => {
  let constraints;
  const mic = stream();
  const f = fixture(async (c) => { constraints = c; return mic; }, 'usb-mic');
  eq(await vm.runInContext('getMicrophoneStream()', f.context), mic);
  eq(constraints.audio.deviceId, { exact: 'usb-mic' });
  eq(constraints.video, false);
  assert(constraints.audio.echoCancellation && constraints.audio.noiseSuppression);
});

test('recorder: system-default microphone does not pin a stale device id', async () => {
  let constraints;
  const f = fixture(async (c) => { constraints = c; return stream(); });
  await vm.runInContext('getMicrophoneStream()', f.context);
  assert(!('deviceId' in constraints.audio));
});

for (const name of ['NotAllowedError', 'NotFoundError', 'NotReadableError', 'OverconstrainedError']) {
  test('recorder: ' + name + ' reports microphone failure and releases screen capture', async () => {
    const f = fixture(async () => { throw { name }; });
    await vm.runInContext('window.__shotikStart()', f.context);
    eq(f.errors, ['microphone:' + name]);
    eq(f.display.track.readyState, 'ended');
    eq(f.done.length, 0);
  });
}

test('recorder: empty microphone stream is an error, not a silent video', async () => {
  const empty = stream('video');
  const f = fixture(async () => empty);
  await vm.runInContext('window.__shotikStart()', f.context);
  eq(f.errors, ['microphone:Error']);
  eq(empty.track.readyState, 'ended');
});

test('recorder: stopping during microphone startup releases a late stream', async () => {
  let resolveMic, opening;
  const opened = new Promise((resolve) => { opening = resolve; });
  const f = fixture(() => { opening(); return new Promise((resolve) => { resolveMic = resolve; }); });
  const start = vm.runInContext('window.__shotikStart()', f.context);
  await opened;
  f.stop();
  const mic = stream(); resolveMic(mic);
  await start;
  eq(mic.track.readyState, 'ended');
  eq(f.display.track.readyState, 'ended');
  eq(f.errors.length, 0);
  eq(f.done, [{ duration: 0, canceled: true }]);
});

// ---- audio-only (calls): the real capture path, which ghost mode does not take ----
class FakeStream {
  constructor(tracks = []) { this.tracks = [...tracks]; }
  getTracks() { return [...this.tracks]; }
  getAudioTracks() { return this.tracks.filter((t) => t.kind === 'audio'); }
  getVideoTracks() { return this.tracks.filter((t) => t.kind === 'video'); }
  addTrack(t) { this.tracks.push(t); }
  removeTrack(t) { this.tracks = this.tracks.filter((x) => x !== t); }
}
const track = (kind) => ({ kind, readyState: 'live', stop() { this.readyState = 'ended'; } });

function audioFixture({ system, microphone, displayAudio = true, supported = (m) => m.startsWith('audio/mp4') }) {
  const display = { video: track('video'), audio: track('audio') };
  const mic = track('audio');
  const calls = { display: [], user: [] };
  const errors = [], ready = [];
  let control, recorder;
  class FakeRecorder {
    constructor(stream, opts) { this.stream = stream; this.opts = opts; this.state = 'inactive'; recorder = this; }
    static isTypeSupported(m) { return supported(m); }
    start() { this.state = 'recording'; }
    stop() { this.state = 'inactive'; if (this.onstop) this.onstop(); }
  }
  class FakeAudioContext {
    constructor() { this.state = 'running'; }
    createAnalyser() { return { fftSize: 1024, getFloatTimeDomainData() {} }; }
    createMediaStreamSource() { return { connect: (n) => n }; }
    createMediaStreamDestination() { return { stream: new FakeStream([track('audio')]) }; }
    async resume() {}
    async close() { this.state = 'closed'; }
  }
  const query = `?test=0&audioOnly=1&audio=${system ? 1 : 0}&microphone=${microphone ? 1 : 0}`;
  const context = vm.createContext({
    URLSearchParams, location: { search: query }, MediaStream: FakeStream, MediaRecorder: FakeRecorder, AudioContext: FakeAudioContext,
    navigator: { mediaDevices: {
      getDisplayMedia: async (c) => { calls.display.push(c); return new FakeStream(displayAudio ? [display.video, display.audio] : [display.video]); },
      getUserMedia: async (c) => { calls.user.push(c); return new FakeStream([mic]); },
    } },
    window: { MediaRecorder: FakeRecorder, rec: { trace() {}, level() {}, chunk() {}, firstFrame() {}, error: (e) => errors.push(e), ready: (r) => ready.push(r), done() {}, onControl: (cb) => { control = cb; } } },
    setInterval, clearInterval, setTimeout, clearTimeout, cancelAnimationFrame() {}, performance,
  });
  vm.runInContext(source, context);
  return { context, display, mic, calls, errors, ready, recorder: () => recorder, stop: () => control('stop') };
}

test('recorder: audio-only drops the screen track at once and records AAC .m4a', async () => {
  const f = audioFixture({ system: true, microphone: true });
  await vm.runInContext('window.__shotikStart()', f.context);
  eq(f.errors, []);
  eq(f.display.video.readyState, 'ended', 'screen video stopped right away');
  eq(f.display.audio.readyState, 'live', 'system audio kept');
  eq(f.calls.display[0].video.frameRate, 1, 'smallest possible screen request');
  eq(f.ready[0].ext, 'm4a');
  eq(f.ready[0].audioOnly, true);
  eq(f.recorder().opts, { mimeType: 'audio/mp4;codecs=mp4a.40.2', audioBitsPerSecond: 128000 });
  eq([f.recorder().stream.getVideoTracks().length, f.recorder().stream.getAudioTracks().length], [0, 1], 'one mixed audio track, no video');
  f.stop();
});

test('recorder: microphone-only audio never asks for the screen', async () => {
  const f = audioFixture({ system: false, microphone: true });
  await vm.runInContext('window.__shotikStart()', f.context);
  eq(f.errors, []);
  eq(f.calls.display.length, 0);
  eq(f.ready[0].audio, { system: false, microphone: true, tracks: 1 });
  f.stop();
});

test('recorder: no system audio in the screen stream is an explicit error, not a silent file', async () => {
  const f = audioFixture({ system: true, microphone: false, displayAudio: false });
  await vm.runInContext('window.__shotikStart()', f.context);
  eq(f.errors, ['audio:system-unavailable']);
  eq(f.display.video.readyState, 'ended');
  eq(f.ready.length, 0);
});

test('recorder: without an AAC encoder audio falls back to Opus WebM', async () => {
  const f = audioFixture({ system: false, microphone: true, supported: (m) => m === 'audio/webm;codecs=opus' });
  await vm.runInContext('window.__shotikStart()', f.context);
  eq([f.ready[0].mime, f.ready[0].ext], ['audio/webm;codecs=opus', 'webm']);
  f.stop();
});
