'use strict';
// Regenerates the application screenshots in docs/ from a built binary, in ghost mode, so nothing reaches the desktop.
// The gallery shows the application's own pages: nothing real, and so nothing private, can end up on a picture.
//
//   SHOTIK_BIN=target/release/shotik.exe node docs/make-screenshots.js
const fs = require('fs');
const http = require('http');
const os = require('os');
const path = require('path');
const { makeApp } = require('../test/ghost/adapter');
const { signRelease, publicKeyHex } = require('../installer/sign-update');

const DOCS = __dirname;
const SEED = '07'.repeat(32);
// The toast offers the next minor version, so the picture shows a version that could exist.
const [, major, minor] = fs.readFileSync(path.join(DOCS, '..', 'Cargo.toml'), 'utf8').match(/^version = "(\d+)\.(\d+)\./m);
const NEXT = `${major}.${Number(minor) + 1}.0`;
const output = (name, lang) => path.join(DOCS, lang === 'en' ? `${name}.png` : `${name}.${lang}.png`);

async function ready(app, language, extra = {}) {
  await app.waitReady();
  await app.showMain();
  await app.settingsSet({ language });
  await app.sleep(600);
  return extra;
}

async function page(app, name) {
  await app.uiAction('main', { type: 'navigate', page: name });
  await app.sleep(500);
}

async function theme(app, mode) {
  await app.post('set-theme', { mode });
  await app.sleep(500);
}

/** The pictures the gallery shows: the application's own pages, in both themes. */
async function galleryPictures(lang, dir) {
  // A made-up folder: the settings page shows it, and a temporary path would show the name of whoever ran this.
  const app = makeApp({ settings: { saveDir: 'C:\Users\you\Pictures\Shotik' } });
  try {
    await ready(app, lang);
    const pictures = [];
    for (const [name, mode, source] of [
      ['settings', 'dark', 'region'], ['claude', 'light', 'fullscreen'], ['text', 'dark', 'region'],
      ['settings', 'light', 'region'], ['text', 'light', 'fullscreen'], ['claude', 'dark', 'region'],
    ]) {
      await page(app, name);
      await theme(app, mode);
      const file = path.join(dir, `${name}-${mode}.png`);
      const { size } = await app.capturePage('main', file);
      pictures.push({ file, width: size.width, height: size.height, source });
    }
    return pictures;
  } finally {
    await app.stop();
  }
}

async function shots(lang, pictures, base) {
  const now = Date.now();
  const history = pictures.map((picture, index) => ({
    id: `shot-${index}`, file: picture.file, thumb: null, width: picture.width, height: picture.height,
    ts: now - index * 47 * 60 * 1000, source: picture.source,
  }));
  const app = makeApp({
    history,
    env: { SHOTIK_UPDATE_BASE: base, SHOTIK_UPDATE_PUBLIC_KEY: publicKeyHex(SEED) },
    settings: { checkUpdates: false },
  });
  try {
    await ready(app, lang);
    for (const mode of ['light', 'dark']) {
      await theme(app, mode);
      await page(app, 'history');
      await app.capturePage('main', output(`app-${mode}`, lang));
      await page(app, 'text');
      await app.capturePage('main', output(`app-text-${mode}`, lang));
    }
    await theme(app, 'light');
    await app.post('update-check');
    await app.sleep(500);
    await app.capturePage('toast', output('update-toast', lang));
  } finally {
    await app.stop();
  }
}

(async () => {
  const release = signRelease(Buffer.from('x'), NEXT, SEED);
  const server = http.createServer((request, response) => {
    const body = { '/latest.json': release.manifest, '/latest.json.sig': release.signature }[request.url];
    response.writeHead(body ? 200 : 404);
    response.end(body || '');
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-doc-shots-'));
  try {
    for (const lang of ['en', 'ru']) {
      const folder = path.join(dir, lang);
      fs.mkdirSync(folder);
      await shots(lang, await galleryPictures(lang, folder), base);
      console.log('written for ' + lang);
    }
  } finally {
    server.close();
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 300 });
  }
})().catch((e) => {
  console.error(e.stack || e);
  process.exit(1);
});
