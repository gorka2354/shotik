'use strict';
// Shotik benchmark: one scenario for the Electron and Rust builds.
//
//   node bench/run.js --impl electron --exe "%LOCALAPPDATA%\Programs\shotik\Shotik.exe" --label electron-1.3.0
//   node bench/run.js --impl rust --exe target/release/shotik.exe --label rust-0.1.0
//
// The instance runs in ghost mode: windows off-screen, no hotkeys, the desktop stays untouched.
// Capture is REAL: screen frames and system audio reach only a temporary directory that is deleted
// at the end. The microphone is never enabled.
const { spawn, execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const lib = require('./lib');
const adapters = require('./adapters');

const { sleep, post, mcp, textOf, launch, waitReady, stop, snapshot, cpuRate, stats, parseMarks } = lib;

function arg(name, fallback = null) {
  const i = process.argv.indexOf('--' + name);
  return i === -1 ? fallback : process.argv[i + 1];
}
const flag = (name) => process.argv.includes('--' + name);

const IMPL = adapters[arg('impl', 'electron')];
const EXE = path.resolve(arg('exe') || '');
const LABEL = arg('label', 'unnamed');
const QUICK = flag('quick');
const ROOT = path.join(__dirname, '..');
const PORT = Number(arg('port', 7601));
const OUT = path.resolve(arg('out', path.join(__dirname, 'results', LABEL + '.json')));
const N = QUICK
  ? { startups: 2, overlays: 6, shots: 4, windows: 3, captures: 3, ocr: 2, videoSec: 5, audioSec: 4, idleSec: 5 }
  : { startups: 7, overlays: 30, shots: 20, windows: 15, captures: 15, ocr: 10, videoSec: 15, audioSec: 10, idleSec: 30 };

if (!IMPL) throw new Error('--impl must be electron or rust');
if (!fs.existsSync(EXE)) throw new Error('--exe not found: ' + EXE);

const log = (...a) => console.log(new Date().toISOString().slice(11, 19), ...a);

async function timed(fn) {
  const t = process.hrtime.bigint();
  const value = await fn();
  return { ms: Number(process.hrtime.bigint() - t) / 1e6, value };
}

async function waitFor(check, what, timeoutMs = 30000) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    const v = await check();
    if (v) return v;
    await sleep(50);
  }
  throw new Error('timed out waiting for: ' + what);
}

function machineInfo() {
  const cmd = '$c=Get-CimInstance Win32_Processor;$o=Get-CimInstance Win32_OperatingSystem;'
    + '@{cpu=$c.Name;threads=$c.NumberOfLogicalProcessors;loadPercent=$c.LoadPercentage;'
    + 'ramTotalGB=[math]::Round($o.TotalVisibleMemorySize/1MB);ramFreeGB=[math]::Round($o.FreePhysicalMemory/1MB);os=$o.Caption}|ConvertTo-Json -Compress';
  return JSON.parse(execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', cmd], { encoding: 'utf8' }));
}

function dirSizeMB(dir) {
  let total = 0;
  const walk = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p); else { try { total += fs.statSync(p).size; } catch (_) { /* locked */ } }
    }
  };
  walk(dir);
  return Math.round(total / 1048576);
}

async function benchStartup(results, env) {
  log(`startup: ${N.startups} runs`);
  const times = [];
  const idle = [];
  for (let i = 0; i < N.startups; i++) {
    const app = launch({ exe: EXE, args: IMPL.args, cwd: path.dirname(EXE), port: PORT, env });
    try {
      times.push(await waitReady(app));
      await sleep(5000);
      idle.push(snapshot(app.child.pid));
    } finally { await stop(app); }
    await sleep(800);
  }
  results.startup = { readyMs: stats(times), first: Math.round(times[0]) };
  results.afterStartup5s = { treePrivateMB: stats(idle.map((s) => s.treePrivateMB)), procs: idle[idle.length - 1].procs };
}

async function benchOverlay(results, port, pid) {
  log(`overlay: ${N.overlays} sessions`);
  const before = snapshot(pid);
  const showTimes = [];
  const marksAll = [];
  let duringPrivate = 0;
  for (let i = 0; i < N.overlays; i++) {
    const r = await post(port, 'trigger', { mode: 'region' });
    if (!r.overlayOpen) throw new Error(`overlay did not open (session ${i + 1})`);
    const marks = parseMarks(await post(port, 'overlay-timing', {}));
    showTimes.push(marks.show);
    marksAll.push(marks);
    if (i === 0 || i === N.overlays - 1) duringPrivate = Math.max(duringPrivate, snapshot(pid).treePrivateMB);
    await IMPL.cancelOverlay(port);
    await sleep(150);
  }
  await sleep(10000);
  const after = snapshot(pid);
  const stages = {};
  for (const k of Object.keys(marksAll[marksAll.length - 1])) stages[k] = stats(marksAll.slice(1).map((m) => m[k]));
  results.overlay = {
    coldShowMs: Math.round(showTimes[0]),
    warmShowMs: stats(showTimes.slice(1)),
    warmStagesMs: stages,
    peakTreePrivateMB: duringPrivate,
    treePrivateBeforeMB: before.treePrivateMB,
    treePrivateAfterSettleMB: after.treePrivateMB,
    growthKBPerSession: Math.round(((after.treePrivateMB - before.treePrivateMB) * 1024) / N.overlays),
    note: 'up to show(); in ghost mode the window is off-screen and not focused, on-screen painting is not included',
  };
}

async function benchMcp(results, port, pid, primaryId, testTitle) {
  log(`MCP: take_screenshot ${N.shots}, list_windows ${N.windows}, capture_window ${N.captures}`);
  const before = snapshot(pid);
  const shots = [];
  const shotsFull = [];
  for (let i = 0; i < N.shots; i++) {
    shots.push((await timed(() => mcp(port, 'take_screenshot', { display_id: primaryId }))).ms);
    shotsFull.push((await timed(() => mcp(port, 'take_screenshot', { display_id: primaryId, full_resolution: true }))).ms);
  }
  const lists = [];
  let found = null;
  for (let i = 0; i < N.windows; i++) {
    const r = await timed(() => mcp(port, 'list_windows', {}));
    lists.push(r.ms);
    found = JSON.parse(textOf(r.value)).find((w) => w.title === testTitle) || found;
  }
  const caps = [];
  if (found) {
    for (let i = 0; i < N.captures; i++) {
      caps.push((await timed(() => mcp(port, 'capture_window', { window_id: found.id, title: testTitle }))).ms);
    }
  }
  const after = snapshot(pid);
  results.mcp = {
    takeScreenshotMs: stats(shots),
    takeScreenshotFullResMs: stats(shotsFull),
    listWindowsMs: stats(lists),
    captureWindowMs: stats(caps),
    captureWindowTarget: found ? 'external off-screen test window' : 'window not found, measurement skipped',
    growthKBPerShot: Math.round(((after.treePrivateMB - before.treePrivateMB) * 1024) / (N.shots * 2)),
  };
}

async function benchOcr(results, port) {
  log(`OCR: ${N.ocr} runs`);
  const fixture = path.join(ROOT, 'test', 'fake-screen.png');
  const times = [];
  let length = 0;
  for (let i = 0; i < N.ocr; i++) {
    const r = await timed(() => post(port, 'ocr-selection', { path: fixture }));
    times.push(r.ms);
    length = String(r.value.text || '').length;
  }
  results.ocr = { selectionMs: stats(times), recognizedChars: length, fixture: 'test/fake-screen.png' };
}

async function sampleWhile(pid, seconds) {
  const start = snapshot(pid);
  let peak = start.treePrivateMB;
  const end = Date.now() + seconds * 1000;
  while (Date.now() < end) {
    await sleep(Math.min(2500, Math.max(0, end - Date.now())));
    peak = Math.max(peak, snapshot(pid).treePrivateMB);
  }
  const last = snapshot(pid);
  return { cpuMsPerSec: cpuRate(start, last), peakTreePrivateMB: Math.max(peak, last.treePrivateMB) };
}

async function finishRecording(port) {
  const stopT = process.hrtime.bigint();
  await post(port, 'record-stop', {});
  const done = await waitFor(async () => {
    const st = await post(port, 'state', {});
    return st.lastProcessed && st.lastProcessed.action === 'record-done' && st.lastProcessed;
  }, 'record-done', 60000);
  return { stopMs: Math.round(Number(process.hrtime.bigint() - stopT) / 1e6), file: done.file };
}

async function benchRecording(results, port, pid, display) {
  log(`video recording: ${N.videoSec} s, 1280x720`);
  const scale = display.scaleFactor;
  const rectPhys = { x: 100, y: 100, w: 1280, h: 720 };
  const rectDip = Object.fromEntries(Object.entries(rectPhys).map(([k, v]) => [k, Math.round(v / scale)]));
  await IMPL.setSettings(port, {
    showToast: false,
    recording: { fps: 30, format: 'auto', systemAudio: false, microphone: false, countdown: false },
    audioRecording: { systemAudio: true, microphone: false },
  });
  const startT = process.hrtime.bigint();
  await IMPL.startVideo(port, { displayId: display.id, rectPhys, rectDip });
  await waitFor(async () => {
    const s = await post(port, 'record-status', {});
    return s.recording && s.audio ? s : null;
  }, 'recording running');
  const video = { startMs: Math.round(Number(process.hrtime.bigint() - startT) / 1e6) };
  Object.assign(video, await sampleWhile(pid, N.videoSec));
  const fin = await finishRecording(port);
  video.stopMs = fin.stopMs;
  video.sizeMB = Math.round((fs.statSync(fin.file).size / 1048576) * 10) / 10;
  video.container = path.extname(fin.file).slice(1);
  video.seconds = N.videoSec;
  video.mbPerMinute = Math.round((video.sizeMB / N.videoSec) * 60 * 10) / 10;
  results.video = video;

  log('GIF export');
  const [entry] = await IMPL.historyList(port);
  const gif = await timed(() => IMPL.exportGif(port, entry.id));
  results.gif = {
    exportMs: Math.round(gif.ms),
    sizeMB: gif.value && fs.existsSync(gif.value) ? Math.round((fs.statSync(gif.value).size / 1048576) * 10) / 10 : null,
    ok: !!(gif.value && fs.existsSync(gif.value)),
  };

  log(`audio recording: ${N.audioSec} s, system only`);
  const audioStart = process.hrtime.bigint();
  await post(port, 'trigger', { mode: 'audio' });
  await waitFor(async () => {
    const s = await post(port, 'record-status', {});
    return s.recording && s.audio ? s : null;
  }, 'audio recording running');
  const audio = { startMs: Math.round(Number(process.hrtime.bigint() - audioStart) / 1e6) };
  Object.assign(audio, await sampleWhile(pid, N.audioSec));
  await sleep(50);
  // lastProcessed still holds the video: wait for the new file
  await post(port, 'record-stop', {});
  const audioDone = await waitFor(async () => {
    const st = await post(port, 'state', {});
    return st.lastProcessed && st.lastProcessed.action === 'record-done' && /\.m4a$/i.test(st.lastProcessed.file || '') && st.lastProcessed;
  }, 'audio file', 60000);
  audio.sizeKB = Math.round(fs.statSync(audioDone.file).size / 1024);
  results.audio = audio;
}

async function main() {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-bench-'));
  const saveDir = path.join(tmp, 'saved').replace(/\\/g, '/');
  const results = {
    label: LABEL,
    impl: IMPL.name,
    quick: QUICK,
    measuredAt: new Date().toISOString(),
    machine: machineInfo(),
    artifact: {
      exeMB: Math.round(fs.statSync(EXE).size / 1048576),
      installDirMB: dirSizeMB(path.dirname(EXE)),
    },
  };
  let testWin = null;
  let app = null;
  // The instance's own temp directory: an interrupted recording leaves a part file with real screen
  // content, and it must go away with everything else, without touching the shared %TEMP%.
  const env = { ...IMPL.env, TEMP: tmp, TMP: tmp };
  try {
    await benchStartup(results, env);

    const testTitle = 'ShotikBench-' + Date.now();
    testWin = spawn('powershell.exe',
      ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', path.join(ROOT, 'test', 'ghost', 'testwin.ps1'), '-Title', testTitle],
      { windowsHide: true, stdio: 'ignore' });

    app = launch({ exe: EXE, args: IMPL.args, cwd: path.dirname(EXE), port: PORT, env });
    await waitReady(app);
    const pid = app.child.pid;
    await IMPL.setSettings(PORT, { saveDir });

    const displays = JSON.parse(textOf(await mcp(PORT, 'list_displays', {})));
    results.displays = displays.map((d) => ({ w: d.bounds.width, h: d.bounds.height, scale: d.scaleFactor, primary: d.primary }));
    const primary = displays.find((d) => d.primary);

    log(`idle: ${N.idleSec} s`);
    await sleep(N.idleSec * 1000);
    const idleA = snapshot(pid);
    await sleep(N.idleSec * 1000);
    const idleB = snapshot(pid);
    results.idle = {
      procs: idleB.procs, rootPrivateMB: idleB.rootPrivateMB, treePrivateMB: idleB.treePrivateMB,
      treeWorkingSetMB: idleB.treeWorkingSetMB, cpuMsPerSec: cpuRate(idleA, idleB),
    };

    await benchOverlay(results, PORT, pid);
    await benchMcp(results, PORT, pid, primary.id, testTitle);
    await benchOcr(results, PORT);
    await benchRecording(results, PORT, pid, primary);

    await sleep(10000);
    const end = snapshot(pid);
    results.afterAll = { procs: end.procs, treePrivateMB: end.treePrivateMB, treeWorkingSetMB: end.treeWorkingSetMB };
  } finally {
    if (app) await stop(app);
    if (testWin) { try { testWin.kill(); } catch (_) { /* already closed */ } }
    // screen frames, recordings and audio must not stay on disk
    await sleep(500);
    try { fs.rmSync(tmp, { recursive: true, force: true }); } catch (e) { console.error('could not delete', tmp, e.message); }
  }

  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(results, null, 2) + '\n', 'utf8');
  log('done:', OUT);
  console.log(JSON.stringify(results, null, 2));
}

main().catch((e) => { console.error('BENCHMARK FAILED:', e.stack || e.message); process.exit(1); });
