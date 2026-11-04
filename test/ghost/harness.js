'use strict';
// Private ghost instances only. Never attach to an already listening test port.
const { spawn, execFileSync } = require('child_process');
const fs = require('fs');
const http = require('http');
const net = require('net');
const os = require('os');
const path = require('path');
const { preserveClipboard } = require('./clipboard');

const ROOT = path.join(__dirname, '..', '..');
const ELECTRON = path.join(ROOT, 'node_modules', 'electron', 'dist', 'electron.exe');

function makeApp({ port = 0, env = {}, impl = process.env.SHOTIK_IMPL || 'electron', settings = {}, history = [] } = {}) {
  if (!['electron', 'rust'].includes(impl)) throw new Error('SHOTIK_IMPL must be electron or rust');
  if (impl === 'rust' && !process.env.SHOTIK_BIN) throw new Error('SHOTIK_IMPL=rust requires SHOTIK_BIN');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-ghost-'));
  const appData = path.join(dir, 'appdata');
  const profile = path.join(appData, 'Shotik-Test');
  const temp = path.join(dir, 'temp');
  const saveDir = path.join(dir, 'shots');
  for (const d of [profile, temp, saveDir]) fs.mkdirSync(d, { recursive: true });
  fs.writeFileSync(path.join(profile, 'settings.json'), JSON.stringify({ saveDir, ...settings }));
  fs.writeFileSync(path.join(profile, 'history.json'), JSON.stringify(history));
  // Electron resolves appData using Windows known folders, ignoring APPDATA.
  // Apply private paths in the entry point before loading frozen src/.
  const pkg = require('../../package.json');
  fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({
    name: pkg.name, productName: pkg.productName, version: pkg.version,
    main: path.join(__dirname, 'electron-entry.js'),
  }));
  const childEnv = {
    ...process.env, ...env,
    APPDATA: appData, TEMP: temp, TMP: temp,
    SHOTIK_TEST: '1', SHOTIK_GHOST: '1', SHOTIK_TRANSLATE_STUB: '1',
    SHOTIK_PORT: String(port), SHOTIK_SAVE_DIR: saveDir,
    SHOTIK_FAKE_SCREEN: path.join(ROOT, 'test', 'fake-screen.png'),
    SHOTIK_REAL_CAPTURE: '0', SHOTIK_POPUP_REAL: '0',
  };
  delete childEnv.ELECTRON_RUN_AS_NODE;
  let child = null, stopped = false, clipboardGuard = null, ownsListener = false;

  function listenerPid() {
    const out = execFileSync('netstat.exe', ['-ano', '-p', 'tcp'],
      { encoding: 'utf8', windowsHide: true, timeout: 10000 });
    const listener = out.split(/\r?\n/).map((line) => line.trim().split(/\s+/))
      .find((parts) => parts[1] === '127.0.0.1:' + port && parts[2] === '0.0.0.0:0');
    return listener ? Number(listener[listener.length - 1]) : null;
  }

  const post = (route, body) => new Promise((resolve, reject) => {
    const data = Buffer.from(JSON.stringify(body || {}));
    const req = http.request({ host: '127.0.0.1', port, path: '/test/' + route, method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Content-Length': data.length } },
      (res) => { let b = ''; res.on('data', (c) => (b += c)); res.on('end', () => {
        try {
          const j = JSON.parse(b);
          if (res.statusCode !== 200 || j.ok !== true) throw new Error(j.error || 'HTTP ' + res.statusCode);
          resolve(j.result);
        } catch (e) { reject(new Error('/test/' + route + ': ' + e.message)); }
      }); });
    req.setTimeout(60000, () => req.destroy(new Error('test route timeout: ' + route)));
    req.on('error', reject); req.end(data);
  });

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  async function waitReady(timeoutMs = 30000) {
    if (!child) {
      // Check ownership before starting anything; never ping/quit a neighbour.
      await new Promise((resolve, reject) => {
        const server = net.createServer();
        server.once('error', () => reject(new Error('test port is occupied: ' + port)));
        server.listen(port, '127.0.0.1', () => {
          port = server.address().port;
          childEnv.SHOTIK_PORT = String(port);
          server.close(resolve);
        });
      });
      clipboardGuard = await preserveClipboard();
      child = spawn(impl === 'electron' ? ELECTRON : process.env.SHOTIK_BIN,
        impl === 'electron' ? [dir, '--hidden', '--test'] : ['--hidden', '--test'],
        { cwd: ROOT, env: childEnv, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
      child.stdout.resume(); child.stderr.resume();
      child.on('error', (e) => { child.launchError = e; });
    }
    const end = Date.now() + timeoutMs;
    while (Date.now() < end) {
      if (child.launchError) throw child.launchError;
      if (child.exitCode !== null) throw new Error(impl + ' exited before ready: ' + child.exitCode);
      const owner = listenerPid();
      if (owner && owner !== child.pid) throw new Error('test port belongs to another process: ' + port);
      if (owner === child.pid) {
        ownsListener = true;
        try { if (await post('ping', {}) === 'pong') return; } catch (_) {}
      }
      await sleep(200);
    }
    throw new Error('app did not become ready on port ' + port);
  }

  async function stop() {
    if (stopped) return;
    stopped = true;
    try {
      if (child && child.pid && child.exitCode === null && child.signalCode === null) {
        // Recheck after a startup failure or early exit: never send quit to a
        // process that acquired the port between allocation and app startup.
        if (ownsListener && listenerPid() === child.pid) {
          try { await post('quit'); } catch (_) {}
        }
        for (let i = 0; i < 20 && child.exitCode === null; i++) await sleep(100);
        if (child.exitCode === null && child.signalCode === null) {
          // Only the tree we launched, never the user's Shotik or other tests.
          execFileSync('taskkill', ['/F', '/T', '/PID', String(child.pid)], { stdio: 'ignore', windowsHide: true });
          await sleep(200);
        }
      }
    } finally {
      try { if (clipboardGuard) await clipboardGuard.restore(); }
      finally { fs.rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }); }
    }
  }

  return { post, waitReady, stop, sleep, impl, dir, profile, saveDir, temp, childEnv,
    get port() { return port; },
    get child() { return child; } };
}

module.exports = { makeApp, ELECTRON };
