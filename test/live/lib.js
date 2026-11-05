'use strict';
// Shared parts of the live check. Every input goes through a guard: if the user touches the machine, or the
// target is not this test's own window, nothing is sent and the whole check stops.
const { spawn, execFileSync } = require('child_process');
const fs = require('fs');
const http = require('http');
const os = require('os');
const path = require('path');
const readline = require('readline');

const ROOT = path.join(__dirname, '..', '..');
const SCRATCH = __dirname;
// Survives between runs, so a run can tell its own last input from the user's.
const LAST_RUN = path.join(os.tmpdir(), 'shotik-live-last.json');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

class UserBack extends Error {}

class Probe {
  constructor() {
    this.child = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File',
      path.join(SCRATCH, 'probe.ps1')], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    this.lines = readline.createInterface({ input: this.child.stdout });
    this.waiters = [];
    this.lines.on('line', (l) => { const w = this.waiters.shift(); if (w) w(l); });
    this.child.stderr.on('data', (d) => process.stderr.write('[probe] ' + d));
  }
  ready() { return new Promise((resolve) => this.waiters.push(resolve)); }
  async call(op, args = {}) {
    const text = JSON.stringify({ op, ...args }).replace(/[\u0080-\uffff]/g, (c) => '\\u' + c.charCodeAt(0).toString(16).padStart(4, '0'));
    const reply = new Promise((resolve) => this.waiters.push(resolve));
    this.child.stdin.write(text + '\n');
    const line = await reply;
    const parsed = JSON.parse(line);
    if (!parsed.ok) throw new Error(`probe ${op}: ${parsed.error}`);
    return parsed.result;
  }
  close() { try { this.child.stdin.end(); } catch (_) {} }
}

function post(port, route, body = {}, timeoutMs = 60000) {
  return new Promise((resolve, reject) => {
    const data = Buffer.from(JSON.stringify(body));
    const req = http.request({ host: '127.0.0.1', port, path: '/test/' + route, method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Content-Length': data.length } }, (res) => {
      let text = '';
      res.on('data', (c) => (text += c));
      res.on('end', () => {
        try {
          const j = JSON.parse(text);
          if (j.ok !== true) throw new Error(j.error || 'HTTP ' + res.statusCode);
          resolve(j.result);
        } catch (e) { reject(new Error(`/test/${route}: ${e.message}`)); }
      });
    });
    req.setTimeout(timeoutMs, () => req.destroy(new Error('timeout: ' + route)));
    req.on('error', reject);
    req.end(data);
  });
}

const VK = { ctrl: 0x11, alt: 0x12, shift: 0x10, esc: 0x1B, enter: 0x0D, left: 0x25, up: 0x26, right: 0x27, down: 0x28,
  f4: 0x73, f5: 0x74, f6: 0x75, f7: 0x76, f8: 0x77, f9: 0x78, f10: 0x79, f11: 0x7A,
  a: 0x41, c: 0x43, f: 0x46, p: 0x50, r: 0x52, s: 0x53, t: 0x54, y: 0x59, z: 0x5A };
const digit = (n) => 0x30 + n;
const LDOWN = 0x0002, LUP = 0x0004, RDOWN = 0x0008, RUP = 0x0010, WHEEL = 0x0800;

class Live {
  constructor({ minIdleSec = 120 } = {}) {
    this.probe = new Probe();
    this.minIdleSec = minIdleSec;
    this.results = [];
    this.lastInjected = 0;
    this.baseline = 0;
    this.buttonDown = false;
  }

  async begin() {
    await this.probe.ready();
    const t = await this.probe.call('tick');
    // The last input may be this script's own, from the previous run: then the user's is the one before it.
    let userInput = t.lastInput;
    try {
      const saved = JSON.parse(fs.readFileSync(LAST_RUN, 'utf8'));
      if (t.lastInput >= saved.baseline && t.lastInput <= saved.lastInjected + 800) userInput = saved.baseline;
    } catch (_) {}
    const idle = (t.tick - userInput) / 1000;
    if (idle < this.minIdleSec) throw new UserBack(`user active ${Math.round(idle)} s ago, need ${this.minIdleSec} s of idle`);
    this.baseline = t.lastInput;
    this.userInput = userInput;
    this.prior = await this.probe.call('fg');
    return { idle, priorApp: this.prior.cls };
  }

  /** Stops everything if input arrived that this script did not send. */
  async guard() {
    const t = await this.probe.call('tick');
    const expected = Math.max(this.baseline, this.lastInjected + 150);
    if (t.lastInput > expected) throw new UserBack(`input ${t.lastInput - this.lastInjected} ms after the last injected one`);
  }

  mark(reply) {
    this.lastInjected = reply.tick;
    fs.writeFileSync(LAST_RUN, JSON.stringify({ baseline: this.userInput, lastInjected: this.lastInjected }));
  }

  /** A global chord: the script's private hotkeys. Harmless if nothing owns them. */
  async chord(...vks) { await this.guard(); this.mark(await this.probe.call('key', { vks })); }

  /** A key for this test's own focused window only. */
  async key(pid, ...vks) {
    await this.guard();
    const fg = await this.probe.call('fg');
    if (fg.pid !== pid) throw new Error(`refused key: foreground is ${fg.cls}, not the tested app`);
    this.mark(await this.probe.call('key', { vks }));
  }

  async type(pid, text) {
    await this.guard();
    const fg = await this.probe.call('fg');
    if (fg.pid !== pid) throw new Error(`refused typing: foreground is ${fg.cls}`);
    this.mark(await this.probe.call('type', { text }));
  }

  /** Moving the pointer clicks nothing. */
  async move(x, y) { await this.guard(); this.mark(await this.probe.call('move', { x, y })); }

  async assertTarget(pids, x, y) {
    const w = await this.probe.call('at', { x, y });
    if (!pids.includes(w.pid)) throw new Error(`refused click at ${x},${y}: the window there is ${w.cls}`);
    return w;
  }

  /** The last check before a button goes down: the cursor is where the target was checked. */
  async assertCursor(x, y) {
    const c = await this.probe.call('cursor');
    if (Math.abs(c.x - x) > 2 || Math.abs(c.y - y) > 2) throw new UserBack(`the cursor is at ${c.x},${c.y}, it was sent to ${x},${y}`);
  }

  async click(pids, x, y, { button = 'left', twice = false } = {}) {
    await this.assertTarget(pids, x, y);
    await this.move(x, y);
    await sleep(60);
    await this.assertTarget(pids, x, y);
    const [d, u] = button === 'left' ? [LDOWN, LUP] : [RDOWN, RUP];
    for (let i = 0; i < (twice ? 2 : 1); i++) {
      await this.guard();
      await this.assertCursor(x, y);
      this.mark(await this.probe.call('btn', { flags: d }));
      await sleep(25);
      this.mark(await this.probe.call('btn', { flags: u }));
      await sleep(twice ? 60 : 30);
    }
  }

  async drag(pids, x0, y0, x1, y1, { steps = 8, pause = 16 } = {}) {
    await this.assertTarget(pids, x0, y0);
    await this.move(x0, y0);
    await sleep(60);
    await this.assertTarget(pids, x0, y0);
    await this.guard();
    await this.assertCursor(x0, y0);
    this.mark(await this.probe.call('btn', { flags: LDOWN }));
    this.buttonDown = true;
    try {
      for (let i = 1; i <= steps; i++) {
        await this.guard();
        this.mark(await this.probe.call('move', { x: Math.round(x0 + ((x1 - x0) * i) / steps), y: Math.round(y0 + ((y1 - y0) * i) / steps) }));
        await sleep(pause);
      }
    } finally {
      this.mark(await this.probe.call('btn', { flags: LUP }));
      this.buttonDown = false;
    }
    await sleep(60);
  }

  async wheel(pids, x, y, clicks) {
    await this.assertTarget(pids, x, y);
    await this.move(x, y);
    await sleep(40);
    await this.guard();
    this.mark(await this.probe.call('btn', { flags: WHEEL, data: (clicks * 120) >>> 0 }));
    await sleep(80);
  }

  check(name, ok, detail = '') {
    this.results.push({ name, ok: !!ok, detail });
    console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  [' + detail + ']' : ''}`);
    return !!ok;
  }
  note(text) { console.log('      ' + text); }

  summary() {
    const failed = this.results.filter((r) => !r.ok);
    console.log(`\n${this.results.length - failed.length} passed, ${failed.length} failed`);
    for (const f of failed) console.log('  FAILED: ' + f.name + (f.detail ? ' [' + f.detail + ']' : ''));
    return failed.length;
  }
}

async function until(fn, timeoutMs = 3000, step = 40) {
  const end = Date.now() + timeoutMs;
  let last;
  while (Date.now() < end) {
    last = await fn();
    if (last) return last;
    await sleep(step);
  }
  return last;
}

/**
 * The tray icon with a real mouse, through the shell. Windows hides a new icon in a flyout, and the rectangle it
 * reports is then the arrow's, which moves as other icons come and go: so it is read right before the click.
 */
function trayIcon(live, pid) {
  const call = (op, a) => live.probe.call(op, a);
  const click = async (x, y, down, up) => {
    await live.move(x, y);
    await sleep(300);
    await live.guard();
    await live.assertCursor(x, y);
    live.mark(await call('btn', { flags: down }));
    await sleep(60);
    live.mark(await call('btn', { flags: up }));
  };
  const find = () => {
    const out = execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', path.join(SCRATCH, 'tray-icon.ps1')], { encoding: 'utf8', windowsHide: true });
    return JSON.parse(out.trim().split(/\r?\n/).pop());
  };
  const reveal = async () => {
    const shown = find();
    if (shown.found) return shown;
    const arrow = await call('tray', { cls: 'Shotik.NativeTray', pid });
    await click(arrow.x + Math.floor(arrow.w / 2), arrow.y + Math.floor(arrow.h / 2), LDOWN, LUP);
    await until(async () => /Overflow/.test((await call('fg')).cls), 2000, 50);
    await sleep(250);
    return find();
  };
  return {
    reveal,
    leftClick: (icon) => click(icon.x, icon.y, LDOWN, LUP),
    rightClick: (icon) => click(icon.x, icon.y, RDOWN, RUP),
  };
}

/** A private instance of the app with real windows, real capture and its own profile. */
async function launchApp({ exe, port, hotkeys, settings = {}, env = {}, dir }) {
  const appData = path.join(dir, 'appdata');
  const profile = path.join(appData, 'Shotik-Test');
  const temp = path.join(dir, 'temp');
  const saved = path.join(dir, 'saved');
  for (const d of [profile, temp, saved]) fs.mkdirSync(d, { recursive: true });
  fs.writeFileSync(path.join(profile, 'settings.json'), JSON.stringify({ saveDir: saved, hotkeys, ...settings }));
  fs.writeFileSync(path.join(profile, 'history.json'), '[]');
  const childEnv = { ...process.env, APPDATA: appData, TEMP: temp, TMP: temp, SHOTIK_TEST: '1', SHOTIK_PORT: String(port),
    SHOTIK_SAVE_DIR: saved, SHOTIK_REAL_CAPTURE: '1', ...env };
  for (const k of ['SHOTIK_GHOST', 'SHOTIK_FAKE_SCREEN', 'SHOTIK_TRANSLATE_STUB', 'SHOTIK_FORCE_CONTROLS']) delete childEnv[k];
  const errOut = process.env.LIVE_STDERR ? fs.openSync(process.env.LIVE_STDERR, 'w') : 'ignore';
  const child = spawn(exe, ['--hidden', '--test'], { env: childEnv, stdio: ['ignore', errOut, errOut], windowsHide: true });
  const end = Date.now() + 15000;
  for (;;) {
    try { if (await post(port, 'ping') === 'pong') break; } catch (_) {}
    if (Date.now() > end) throw new Error('the app did not answer');
    await sleep(100);
  }
  return { child, pid: child.pid, env: childEnv, saved, temp, dir, profile };
}

async function stopApp(port, app) {
  try { await post(port, 'quit', {}, 3000); } catch (_) {}
  for (let i = 0; i < 30 && app.child.exitCode === null; i++) await sleep(100);
  if (app.child.exitCode === null) {
    try { execFileSync('taskkill', ['/F', '/T', '/PID', String(app.pid)], { stdio: 'ignore', windowsHide: true }); } catch (_) {}
    await sleep(200);
  }
}

module.exports = { Live, Probe, UserBack, trayIcon, post, until, sleep, launchApp, stopApp, VK, digit, ROOT, SCRATCH, WHEEL, os, fs, path };
