'use strict';
// Checking for updates in the real binary, over real WinHTTP, against a local server. No installer here: that part runs
// in installer/update-smoke.js.
const http = require('http');
const { makeApp } = require('./adapter');
const { test, assert, eq } = require('../tiny');
const { signRelease, publicKeyHex } = require('../../installer/sign-update');

const SEED = '07'.repeat(32);
const THIEF = '09'.repeat(32);

async function serve(files) {
  const requests = [];
  const server = http.createServer((req, res) => {
    requests.push(req.url);
    const body = files[req.url];
    res.writeHead(body ? 200 : 404);
    res.end(body || '');
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return { requests, base: `http://127.0.0.1:${server.address().port}`, close: () => server.close() };
}

function release(version, seed = SEED) {
  const { manifest, signature } = signRelease(Buffer.from('installer'), version, seed);
  return { '/latest.json': manifest, '/latest.json.sig': signature };
}

async function withApp(files, settings, work) {
  const server = await serve(files);
  const app = makeApp({
    settings,
    env: { SHOTIK_UPDATE_BASE: server.base, SHOTIK_UPDATE_PUBLIC_KEY: publicKeyHex(SEED) },
  });
  try {
    await app.waitReady();
    await work(app, server);
  } finally {
    await app.stop();
    server.close();
  }
}

async function until(probe, what) {
  for (let i = 0; i < 100; i++) {
    const value = await probe();
    if (value) return value;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error('timed out waiting for ' + what);
}

test('update: a signed newer release reaches the toast, the main window and the settings row', async () => {
  await withApp(release('99.0.0'), {}, async (app) => {
    const status = await until(async () => {
      const s = await app.post('update-status');
      return s.available && s;
    }, 'the scheduled check');
    eq(status.available, '99.0.0');
    const toast = await app.post('last-toast');
    eq(toast.kind, 'update');
    assert(toast.title.includes('99.0.0'), toast.title);
    assert(!!toast.action, 'the toast has a button');
    await app.post('show-main');
    const main = await until(async () => {
      const s = await app.uiState('main');
      return s.updateButton && s;
    }, 'the main window');
    assert(main.updateButton.includes('99.0.0'), main.updateButton);
  });
});

test('update: a release signed with another key is refused', async () => {
  await withApp(release('99.0.0', THIEF), {}, async (app) => {
    const status = await until(async () => {
      const s = await app.post('update-status');
      return s.error && s;
    }, 'the check to fail');
    assert(/signature/.test(status.error), status.error);
    eq(status.available, null);
    eq(status.checked, false);
    eq(await app.post('last-toast'), null);
  });
});

test('update: the same or an older version is not offered', async () => {
  await withApp(release('1.0.0'), {}, async (app) => {
    const status = await until(async () => {
      const s = await app.post('update-status');
      return s.checked && s;
    }, 'the check');
    eq(status.available, null);
    eq(await app.post('last-toast'), null, 'nothing to bother the user with');
  });
});

test('update: an unreachable server stays in the log unless the user asked', async () => {
  await withApp({}, {}, async (app) => {
    const status = await until(async () => {
      const s = await app.post('update-status');
      return s.error && s;
    }, 'the check to fail');
    assert(/404/.test(status.error), status.error);
    eq(await app.post('last-toast'), null);
    await app.post('update-check');
    eq((await app.post('last-toast')).kind, 'update-failed');
  });
});

test('update: with the setting off nothing is asked until the user asks', async () => {
  await withApp(release('99.0.0'), { checkUpdates: false }, async (app, server) => {
    await new Promise((r) => setTimeout(r, 4000));
    eq(server.requests, [], 'nothing leaves the machine');
    const status = await app.post('update-check');
    eq(status.available, '99.0.0');
    assert(server.requests.includes('/latest.json'));
  });
});

test('update: the settings row reports the check, and the switch is stored', async () => {
  await withApp(release('99.0.0'), { checkUpdates: false }, async (app) => {
    await app.showMain();
    const first = await app.uiState('main');
    assert(/not checked yet|ещё не проверялось/.test(first.updateSummary), first.updateSummary);
    eq(first.updateButton, '', 'nothing on offer yet');
    await app.uiAction('main', { type: 'update-check' });
    const found = await until(async () => {
      const s = await app.uiState('main');
      return s.updateSummary.includes('99.0.0') && s;
    }, 'the row to say what it found');
    assert(found.updateButton.includes('99.0.0'), found.updateButton);
    const saved = await app.settingsSet({ checkUpdates: true });
    eq(saved.settings.checkUpdates, true);
  });
});
