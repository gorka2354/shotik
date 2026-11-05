'use strict';
// Publishes a draft release: signs the installer you checked and makes it the update every installed copy receives. Run
// it on the machine that holds the key. Why the key is not on GitHub: see docs/rust/UPDATES.md.
//
//   node installer/publish-release.js v2.1.0 --installer <the file you installed and checked> --notes-file <file>
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

const sha256 = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');

function gh(...args) {
  return execFileSync('gh', [...args, '--repo', REPO], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'], windowsHide: true });
}

function signingKey() {
  const file = path.join(__dirname, '..', '.env');
  const found = fs.existsSync(file) && fs.readFileSync(file, 'utf8').match(/^SHOTIK_UPDATE_KEY=(\S+)\s*$/m);
  if (!found) throw new Error('no SHOTIK_UPDATE_KEY in ' + file);
  return found[1];
}

/**
 * Only the file that was checked may be signed. Whoever controls the repository can swap the draft's copy at any moment
 * after the check, and a signature on their file is exactly what the key is kept off GitHub to deny them.
 */
function assertSameFile(tested, drafted) {
  const [mine, theirs] = [sha256(tested), sha256(drafted)];
  if (mine !== theirs) {
    throw new Error(`the installer on the draft is not the file you checked: ${theirs} against ${mine}. `
      + 'Nothing was signed. Find out who changed it before going on.');
  }
  return mine;
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

async function main([tag, ...rest]) {
  const flags = {};
  for (let i = 0; i < rest.length; i += 2) flags[rest[i]] = rest[i + 1];
  const { '--installer': tested, '--notes-file': notes } = flags;
  if (!/^v\d+\.\d+\.\d+$/.test(tag || '') || !tested || !notes) {
    throw new Error('usage: node installer/publish-release.js v2.1.0 --installer <file> --notes-file <file>');
  }
  const version = tag.slice(1);
  const name = `Shotik-Setup-${version}.exe`;
  if (path.basename(tested) !== name) throw new Error(`the installer of ${tag} is named ${name}, not ${path.basename(tested)}`);
  for (const file of [tested, notes]) {
    if (!fs.existsSync(file)) throw new Error('no such file: ' + file);
  }
  const seed = signingKey();
  if (!JSON.parse(gh('release', 'view', tag, '--json', 'isDraft')).isDraft) throw new Error(tag + ' is already published');

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-publish-'));
  try {
    gh('release', 'download', tag, '--pattern', name, '--dir', dir);
    console.log(`${name} sha256 ${assertSameFile(tested, path.join(dir, name))}`);
    const { manifest } = signFiles(tested, version, dir, seed);
    gh('release', 'upload', tag, path.join(dir, 'latest.json'), path.join(dir, 'latest.json.sig'), '--clobber');
    gh('release', 'edit', tag, '--draft=false', '--latest', '--title', `Shotik ${version}`, '--notes-file', notes);
    await checkPublished(manifest);
    console.log(`published ${tag}: installed copies will find it within a day`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

if (require.main === module) {
  main(process.argv.slice(2)).catch((e) => {
    console.error(e.message);
    process.exit(1);
  });
}
module.exports = { assertSameFile };
