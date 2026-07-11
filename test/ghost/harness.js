'use strict';
// Spins up the app in ghost mode (off-screen, no hotkeys, HTTP-driven) and
// exposes post()/waitReady()/stop(). Every window lives at +20000 so nothing
// touches the real desktop.
const { spawn } = require('child_process');
const http = require('http');
const path = require('path');

const ROOT = path.join(__dirname, '..', '..');

function makeApp({ port, env = {} } = {}) {
  const child = spawn(
    path.join(ROOT, 'node_modules', 'electron', 'dist', 'electron.exe'),
    ['.', '--hidden', '--test'],
    { cwd: ROOT, env: {
      ...process.env,
      SHOTIK_TEST: '1', SHOTIK_GHOST: '1', SHOTIK_TRANSLATE_STUB: '1',
      SHOTIK_PORT: String(port),
      SHOTIK_FAKE_SCREEN: path.join(ROOT, 'test', 'fake-screen.png'),
      ...env,
    } }
  );
  child.stdout.on('data', () => {});
  child.stderr.on('data', () => {});

  const post = (route, body) => new Promise((resolve, reject) => {
    const data = Buffer.from(JSON.stringify(body || {}));
    const req = http.request({ host: '127.0.0.1', port, path: '/test/' + route, method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Content-Length': data.length } },
      (res) => { let b = ''; res.on('data', (c) => (b += c)); res.on('end', () => {
        try { const j = JSON.parse(b); resolve(j && typeof j === 'object' && 'result' in j ? j.result : j); }
        catch (_) { resolve(b); } }); });
    req.on('error', reject); req.write(data); req.end();
  });

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  async function waitReady(timeoutMs = 30000) {
    const end = Date.now() + timeoutMs;
    while (Date.now() < end) {
      try { if (await post('ping', {}) === 'pong') return; } catch (_) {}
      await sleep(400);
    }
    throw new Error('app did not become ready on port ' + port);
  }

  async function stop() {
    try { await post('quit', {}); } catch (_) {}
    await sleep(400);
    try { child.kill(); } catch (_) {}
  }

  return { post, waitReady, stop, sleep, child };
}

module.exports = { makeApp };
