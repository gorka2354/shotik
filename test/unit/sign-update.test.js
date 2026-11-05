'use strict';
// The release side of the update format. crates/shotik-core/src/update.rs pins the same bytes on its side, so a change
// to either one fails a test instead of cutting off every installed copy.
const { spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { test, assert, eq } = require('../tiny');
const { signRelease, publicKeyHex } = require('../../installer/sign-update');

const SCRIPT = path.join(__dirname, '..', '..', 'installer', 'sign-update.js');
const SEED = '07'.repeat(32);

test('sign-update: the bytes the application expects', () => {
  const { manifest, signature } = signRelease(Buffer.from('installer'), '2.1.0', SEED);
  eq(publicKeyHex(SEED), 'ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c');
  eq(manifest.toString(), '{"version":"2.1.0","sha256":"9c0d294c05fc1d88d698034609bb81c0c69196327594e4c69d2915c80fd9850c","size":9}');
  eq(signature.toString(), '7e2bdb87f6eaf1cac50117912af4ee445a7454d0185ace13769fefbcd5939ecc4232666effcea97994167b068b933cb8e0fc2d122c77aa1adb33c0b94f086208\n');
});

test('sign-update: refuses what the application would refuse', () => {
  for (const version of ['2.1', 'v2.1.0', '2.1.0-rc1', '2.1.0/../x', '']) {
    let threw = false;
    try { signRelease(Buffer.from('x'), version, SEED); } catch { threw = true; }
    assert(threw, 'version ' + JSON.stringify(version));
  }
  for (const seed of ['', 'abcd', 'zz'.repeat(32), 'AB'.repeat(32)]) {
    let threw = false;
    try { signRelease(Buffer.from('x'), '2.1.0', seed); } catch { threw = true; }
    assert(threw, 'key of ' + seed.length + ' characters');
  }
});

function inTemp(work) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-sign-'));
  try {
    return work(dir);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

function run(args, key) {
  const env = { ...process.env };
  delete env.SHOTIK_UPDATE_KEY;
  if (key !== undefined) env.SHOTIK_UPDATE_KEY = key;
  return spawnSync(process.execPath, [SCRIPT, ...args], { env, encoding: 'utf8', windowsHide: true });
}

test('sign-update: a key other than the shipped one is refused, and says nothing of it', () => inTemp((dir) => {
  const installer = path.join(dir, 'setup.exe');
  fs.writeFileSync(installer, 'x');
  const result = run([installer, '2.1.0', dir], SEED);
  eq(result.status, 1);
  assert(/does not match/.test(result.stderr), result.stderr);
  assert(!result.stderr.includes(SEED) && !result.stdout.includes(SEED), 'the key must not be printed');
  assert(!fs.existsSync(path.join(dir, 'latest.json')), 'no release without a signature it can stand behind');
}));

test('sign-update: no key is an error, not an unsigned release', () => inTemp((dir) => {
  const installer = path.join(dir, 'setup.exe');
  fs.writeFileSync(installer, 'x');
  const result = run([installer, '2.1.0', dir]);
  eq(result.status, 1);
  assert(/not set/.test(result.stderr), result.stderr);
}));

test('sign-update: --keygen writes the key to the file only, and never replaces one', () => inTemp((dir) => {
  const envFile = path.join(dir, '.env');
  const first = run(['--keygen', envFile]);
  eq(first.status, 0);
  const stored = fs.readFileSync(envFile, 'utf8');
  const seed = stored.match(/^SHOTIK_UPDATE_KEY=([0-9a-f]{64})\n$/)[1];
  eq(first.stdout.trim(), publicKeyHex(seed));
  assert(!first.stdout.includes(seed), 'the private key must not be printed');
  const second = run(['--keygen', envFile]);
  eq(second.status, 1);
  eq(fs.readFileSync(envFile, 'utf8'), stored);
}));
