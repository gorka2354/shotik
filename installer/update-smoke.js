'use strict';
// The whole update, run for real against a local server: a copy put in place by the installer finds a signed release,
// refuses a tampered download, takes the good one, replaces itself and comes back. Private folders, a private profile
// and ghost mode, so it needs no desktop.
//
//   node installer/update-smoke.js <Setup.exe>
//
// The installer writes shortcuts and registry entries for whatever it installs. On a machine with a real install, give
// this one built with /DNO_SHELL_ENTRIES.
const { spawn, spawnSync, execFileSync } = require('child_process');
const crypto = require('crypto');
const fs = require('fs');
const http = require('http');
const net = require('net');
const os = require('os');
const path = require('path');
const { signRelease, publicKeyHex } = require('./sign-update');

const SEED = '07'.repeat(32);
const VERSION = '99.0.0';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const sha256 = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');

function check(condition, what) {
  if (!condition) throw new Error('FAILED: ' + what);
  console.log('ok: ' + what);
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

async function until(what, probe, timeoutMs = 60000) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    try {
      const value = await probe();
      if (value) return value;
    } catch (e) {
      last = e;
    }
    await sleep(200);
  }
  throw new Error(`timed out waiting for ${what}${last ? ': ' + last.message : ''}`);
}

async function main(setup) {
  if (!setup || !fs.existsSync(setup)) throw new Error('usage: node installer/update-smoke.js <Setup.exe>');
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik update smoke '));
  const inst = path.join(root, 'Program Files', 'shotik app');
  const exe = path.join(inst, 'Shotik.exe');
  const appData = path.join(root, 'appdata');
  const temp = path.join(root, 'temp');
  const saves = path.join(root, 'shots');
  for (const dir of [path.join(appData, 'Shotik-Test'), temp, saves]) fs.mkdirSync(dir, { recursive: true });

  // A release of the installer under test, under a version any build counts as newer.
  const installer = fs.readFileSync(setup);
  const release = signRelease(installer, VERSION, SEED);
  let served = installer;
  const server = http.createServer((req, res) => {
    const body = {
      '/latest.json': release.manifest,
      '/latest.json.sig': release.signature,
      [`/Shotik-Setup-${VERSION}.exe`]: served,
    }[req.url];
    res.writeHead(body ? 200 : 404);
    res.end(body || '');
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));

  const port = await freePort();
  const env = {
    ...process.env,
    APPDATA: appData, TEMP: temp, TMP: temp,
    SHOTIK_TEST: '1', SHOTIK_GHOST: '1', SHOTIK_PORT: String(port), SHOTIK_SAVE_DIR: saves,
    SHOTIK_FAKE_SCREEN: path.join(__dirname, '..', 'test', 'fake-screen.png'),
    SHOTIK_UPDATE_BASE: `http://127.0.0.1:${server.address().port}`,
    SHOTIK_UPDATE_PUBLIC_KEY: publicKeyHex(SEED),
  };

  const post = (route, body) => new Promise((resolve, reject) => {
    const data = Buffer.from(JSON.stringify(body || {}));
    const req = http.request({
      host: '127.0.0.1', port, path: '/test/' + route, method: 'POST', timeout: 20000,
      headers: { 'content-type': 'application/json', 'content-length': data.length },
    }, (res) => {
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () => {
        const text = Buffer.concat(chunks).toString();
        try {
          const reply = JSON.parse(text);
          if (reply.ok) resolve(reply.result);
          else reject(new Error(`${route}: ${reply.error}`));
        } catch {
          reject(new Error(`${route}: ${res.statusCode} ${text}`));
        }
      });
    });
    req.on('timeout', () => req.destroy(new Error(route + ': timed out')));
    req.on('error', reject);
    req.end(data);
  });
  const lastToast = () => post('last-toast');
  const clickUpdate = () => post('ui-action', { target: 'toast', action: { type: 'action' } });
  const downloads = () => fs.readdirSync(temp).filter((name) => name.startsWith('shotik-update-'));

  /** /D= has to reach NSIS unquoted, whatever the folder is called. */
  function runSetup(...args) {
    const result = spawnSync(setup, args, { env, argv0: `"${setup}"`, windowsVerbatimArguments: true, timeout: 120000 });
    if (result.error) throw result.error;
    return result.status;
  }

  function startApp() {
    const child = spawn(exe, ['--hidden', '--test'], { env, stdio: 'ignore', detached: true });
    child.unref();
    return new Promise((resolve, reject) => {
      child.once('error', reject);
      child.once('spawn', () => resolve(child));
    });
  }

  const exited = (child) => new Promise((resolve) => {
    if (child.exitCode !== null) resolve(child.exitCode);
    else child.once('exit', resolve);
  });

  /** Only processes running from this private folder: the user's own Shotik is not ours to stop. */
  function stopOurs() {
    const script = 'Get-CimInstance Win32_Process -Filter "Name=\'Shotik.exe\'" | Where-Object { $_.ExecutablePath -and '
      + '$_.ExecutablePath.StartsWith($env:SMOKE_ROOT) } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }';
    execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script],
      { env: { ...process.env, SMOKE_ROOT: root }, windowsHide: true, timeout: 30000 });
  }

  try {
    check(runSetup('/S', '/D=' + inst) === 0, 'the installer installs into a folder with spaces in its name');
    check(fs.existsSync(path.join(inst, 'Uninstall Shotik.exe')), 'and leaves the uninstaller that marks an installed copy');
    const original = sha256(exe);
    const marker = new Date('2000-01-01T00:00:00Z');
    fs.utimesSync(exe, marker, marker);

    let app = await startApp();
    await until('the first copy to answer', () => post('ping'));
    const offered = await until('the scheduled check to find the release',
      async () => { const s = await post('update-status'); return s.available && s; });
    check(offered.available === VERSION, 'the scheduled check finds the signed release');
    const toast = await lastToast();
    check(toast.kind === 'update' && !!toast.action, 'and offers it on a toast with a button');

    // A download that is not the signed file.
    served = Buffer.concat([installer, Buffer.from('tampered')]);
    await post('update-check');
    await clickUpdate();
    const refused = await until('the tampered download to be refused',
      async () => { const s = await post('update-status'); return s.error && !s.installing && s; });
    check(/size|hash/.test(refused.error), 'a tampered download is refused: ' + refused.error);
    check(fs.statSync(exe).mtime.getTime() === marker.getTime(), 'the executable stays untouched');
    check(downloads().length === 0, 'nothing of it stays on disk');
    check((await post('ping')) === 'pong', 'the application keeps running');

    // The real one, through the toast's button.
    served = installer;
    await post('update-check');
    await clickUpdate();
    check((await exited(app)) === 0, 'the application leaves by itself, cleanly');
    await until('the new copy to say it was updated', async () => (await lastToast())?.kind === 'update-done', 90000);
    check(true, 'the installer brings it back and it says it was updated');
    check(fs.statSync(exe).mtime.getTime() !== marker.getTime(), 'the executable really was replaced');
    check(sha256(exe) === original, 'with the bytes from the installer');
    check(!fs.existsSync(exe + '.new') && !fs.existsSync(exe + '.old'), 'no working copies left in the install folder');
    let state = await post('state');
    check(state.mainVisible === false, 'a hidden application stays hidden');
    check(state.mcp.running === true, 'the MCP server is back on its port');
    await until('the old download to be removed', () => downloads().length === 0, 30000);
    check(true, 'the new version removes the installer it came from');

    // With the main window open, it comes back open.
    await until('the release to be found again', async () => (await post('update-status')).available);
    await post('show-main');
    await post('update-check');
    await clickUpdate();
    await until('the second update to go through',
      async () => (await lastToast())?.kind === 'update-done' && (await post('state')).mainVisible === true, 90000);
    check(true, 'a main window that was open comes back open');

    // An executable that cannot be replaced: an installer that fails brings the old version back, and says so.
    await post('quit');
    await until('the application to quit', async () => {
      try { await post('ping'); return false; } catch { return true; }
    });
    const before = sha256(exe);
    const lock = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
      '$f = [IO.File]::Open($env:SMOKE_EXE, "Open", "Read", "Read"); "locked"; Start-Sleep -Seconds 120; $f.Close()'],
    { env: { ...process.env, SMOKE_EXE: exe }, stdio: ['ignore', 'pipe', 'ignore'], windowsHide: true });
    await new Promise((resolve, reject) => {
      lock.stdout.once('data', resolve);
      lock.once('exit', () => reject(new Error('could not lock the executable')));
    });
    try {
      check(runSetup('/S', '/RESTART', '/D=' + inst) === 2, 'an installer that cannot replace the executable reports failure');
      check(sha256(exe) === before, 'and leaves the working one in place');
      check(!fs.existsSync(exe + '.new') && !fs.existsSync(exe + '.old'), 'with nothing else left behind');
      await until('the old version to say the update failed',
        async () => (await lastToast())?.kind === 'update-failed', 60000);
      check(true, 'and the old version comes back and says so');
    } finally {
      lock.removeAllListeners('exit');
      lock.kill();
    }
    app = null;
    console.log('update smoke: ok');
  } finally {
    try {
      stopOurs();
    } catch (e) {
      console.error('could not stop the test copies: ' + e.message);
    }
    server.close();
    for (let i = 0; i < 20; i++) {
      try {
        fs.rmSync(root, { recursive: true, force: true });
        break;
      } catch {
        await sleep(500);
      }
    }
  }
}

main(process.argv[2]).then(() => process.exit(0), (e) => {
  console.error(e.message);
  process.exit(1);
});
