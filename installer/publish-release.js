'use strict';
// Publishes a draft release: signs the installer CI built and makes it the update every installed copy receives. Run it
// on the machine that holds the key, after installing that draft and checking it. Why the key is not on GitHub: see
// docs/rust/UPDATES.md.
//
//   node installer/publish-release.js v2.1.0 --notes-file inc/release-notes-2.1.0.md
//
// The key comes from .env at the repository root. `gh` must be signed in to an account that can write to the repository.
const { execFileSync } = require('child_process');
const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { signFiles } = require('./sign-update');

const REPO = 'gorka2354/shotik';
const LATEST = `https://github.com/${REPO}/releases/latest/download`;

function gh(...args) {
  return execFileSync('gh', [...args, '--repo', REPO], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'], windowsHide: true });
}

function signingKey() {
  const file = path.join(__dirname, '..', '.env');
  const found = fs.existsSync(file) && fs.readFileSync(file, 'utf8').match(/^SHOTIK_UPDATE_KEY=(\S+)\s*$/m);
  if (!found) throw new Error('no SHOTIK_UPDATE_KEY in ' + file);
  return found[1];
}

/** What a copy in the field is about to read. Downloads are cached for a while after publishing. */
async function checkPublished(manifest) {
  let seen = null;
  for (let attempt = 0; attempt < 30; attempt++) {
    const response = await fetch(`${LATEST}/latest.json`, { cache: 'no-store' });
    seen = response.ok ? Buffer.from(await response.arrayBuffer()) : `HTTP ${response.status}`;
    if (Buffer.isBuffer(seen) && seen.equals(manifest)) return;
    await new Promise((resolve) => setTimeout(resolve, 5000));
  }
  throw new Error(`published, but ${LATEST}/latest.json is not this release: ${seen}`);
}

async function main([tag, flag, notesFile]) {
  if (!/^v\d+\.\d+\.\d+$/.test(tag || '') || flag !== '--notes-file' || !notesFile) {
    throw new Error('usage: node installer/publish-release.js v2.1.0 --notes-file <file>');
  }
  if (!fs.existsSync(notesFile)) throw new Error('no such notes file: ' + notesFile);
  const version = tag.slice(1);
  const seed = signingKey();
  if (!JSON.parse(gh('release', 'view', tag, '--json', 'isDraft')).isDraft) throw new Error(tag + ' is already published');

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-publish-'));
  try {
    const name = `Shotik-Setup-${version}.exe`;
    gh('release', 'download', tag, '--pattern', name, '--dir', dir);
    const installer = path.join(dir, name);
    console.log(`${name} sha256 ${crypto.createHash('sha256').update(fs.readFileSync(installer)).digest('hex')}`);
    const { manifest } = signFiles(installer, version, dir, seed);
    gh('release', 'upload', tag, path.join(dir, 'latest.json'), path.join(dir, 'latest.json.sig'), '--clobber');
    gh('release', 'edit', tag, '--draft=false', '--latest', '--title', `Shotik ${version}`, '--notes-file', notesFile);
    await checkPublished(manifest);
    console.log(`published ${tag}: installed copies will find it within a day`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

main(process.argv.slice(2)).catch((e) => {
  console.error(e.message);
  process.exit(1);
});
