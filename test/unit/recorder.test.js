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
