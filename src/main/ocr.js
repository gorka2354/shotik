'use strict';
// OCR via Windows built-in Windows.Media.Ocr (PowerShell bridge). Zero deps, offline,
// supports all languages installed in Windows (incl. Russian + English).
const { app, nativeImage } = require('electron');
const { execFile } = require('child_process');
const { performance } = require('perf_hooks');
const fs = require('fs');
const path = require('path');

// PowerShell can't read scripts from inside the asar archive — use the
// unpacked copy when packaged (see build.asarUnpack).
const SCRIPT = path.join(__dirname, 'ocr.ps1').replace('app.asar' + path.sep, 'app.asar.unpacked' + path.sep);

const MAC_SCRIPT = path.join(__dirname, 'ocr-mac.js').replace('app.asar' + path.sep, 'app.asar.unpacked' + path.sep);

function runPowerShell(args) {
  return new Promise((resolve, reject) => {
    execFile(
      'powershell.exe',
      ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', SCRIPT, ...args],
      { timeout: 25000, windowsHide: true, encoding: 'buffer', maxBuffer: 8 * 1024 * 1024 },
      (err, stdout, stderr) => {
        if (err) return reject(new Error('OCR failed: ' + (stderr ? stderr.toString('utf8') : err.message)));
        resolve(stdout.toString('utf8').replace(/\r/g, '').trim());
      }
    );
  });
}

// Accepts a PNG buffer, returns recognized text (string, may be empty).
// `lang` (optional) forces a specific Windows OCR engine (e.g. 'ru', 'en-US').
async function recognize(pngBuffer, lang) {
  const tmp = path.join(app.getPath('temp'), `shotik-ocr-${Date.now()}-${Math.round(performance.now())}.png`);
  fs.writeFileSync(tmp, pngBuffer);
  try {
    if (process.platform === 'darwin') {
      // Apple Vision via JXA (experimental)
      return await new Promise((resolve, reject) => {
        execFile('osascript', ['-l', 'JavaScript', MAC_SCRIPT, tmp],
          { timeout: 25000, maxBuffer: 4 * 1024 * 1024 },
          (err, stdout, stderr) => {
            if (err) return reject(new Error('OCR failed: ' + (stderr || err.message)));
            resolve(String(stdout).trim());
          });
      });
    }
    if (process.platform !== 'win32') throw new Error('OCR is not supported on this platform yet');
    return await runPowerShell(lang ? [tmp, '-Lang', lang] : [tmp]);
  } finally {
    try { fs.unlinkSync(tmp); } catch (_) {}
  }
}

const cyr = (s) => (s.match(/[Ѐ-ӿ]/g) || []).length;
const lat = (s) => (s.match(/[A-Za-z]/g) || []).length;
// tokens that mix letters and digits (e.g. "a06poe", "3anpoc") — the tell-tale of
// the Latin engine transliterating Cyrillic into garbage.
const digitMix = (s) => s.split(/\s+/).filter((t) => /[A-Za-z]/.test(t) && /[0-9]/.test(t)).length;

// OCR a small on-screen selection for translation. Two quality fixes proven by
// test/ghost/ocr.test.js: (1) Windows OCR misses small UI text, so upscale ~2x
// first; (2) a multi-language engine misreads Latin as look-alike Cyrillic
// (pull→рип), so read Cyrillic-first and only keep it when the text really is
// Cyrillic, otherwise use a Latin-only engine for clean Latin output.
async function recognizeForSelection(pngBuffer) {
  if (process.platform !== 'win32') return (await recognize(pngBuffer).catch(() => '')).trim();
  let png = pngBuffer;
  try {
    const img = nativeImage.createFromBuffer(pngBuffer);
    const s = img.getSize();
    if (s.width && s.width < 1400) {
      png = img.resize({ width: s.width * 2, height: s.height * 2, quality: 'best' }).toPNG();
    }
  } catch (_) {}
  const ru = (await recognize(png, 'ru').catch(() => '')).trim();
  const en = (await recognize(png, 'en-US').catch(() => '')).trim();
  const rc = cyr(ru);
  if (rc > 0 && rc >= lat(ru)) {
    // ru reads as Cyrillic — but the ru engine also turns Latin (esp. UPPERCASE:
    // STATION → ТАТЮ) into Cyrillic. If the en engine gives clean Latin of at
    // least the same size, the source was really Latin — trust en.
    if (en && lat(en) >= rc && digitMix(en) === 0) return en;
    return ru;
  }
  return en || ru;                                           // clean Latin (or fall back)
}

// Returns { lines: [ { text, words: [ { text, x, y, w, h } ] } ] } in the
// pixel coordinates of the passed image. Windows only (Live Text mode);
// macOS falls back to a single line with no boxes.
async function recognizeBoxes(pngBuffer, lang) {
  const tmp = path.join(app.getPath('temp'), `shotik-ocrb-${Date.now()}-${Math.round(performance.now())}.png`);
  fs.writeFileSync(tmp, pngBuffer);
  try {
    if (process.platform !== 'win32') {
      const text = await recognize(pngBuffer).catch(() => '');
      return { lines: text ? text.split('\n').map((t) => ({ text: t, words: [] })) : [] };
    }
    const args = lang ? [tmp, '-Boxes', '-Lang', lang] : [tmp, '-Boxes'];
    const out = await runPowerShell(args);
    if (!out) return { lines: [] };
    try { return JSON.parse(out); } catch (_) { return { lines: [] }; }
  } finally {
    try { fs.unlinkSync(tmp); } catch (_) {}
  }
}

// Double-click: return just the word at (relX, relY) in the region's pixel
// coords. Upscales like recognizeForSelection and picks the language the same
// Cyrillic-first way, then returns the word whose box holds the point (nearest
// if none contains it).
async function recognizeWordAt(pngBuffer, relX, relY) {
  if (process.platform !== 'win32') return (await recognize(pngBuffer).catch(() => '')).trim();
  let png = pngBuffer, scale = 1;
  try {
    const img = nativeImage.createFromBuffer(pngBuffer);
    const s = img.getSize();
    if (s.width && s.width < 1400) { png = img.resize({ width: s.width * 2, height: s.height * 2, quality: 'best' }).toPNG(); scale = 2; }
  } catch (_) {}
  const probe = (await recognize(png, 'ru').catch(() => '')).trim();
  const lang = (cyr(probe) > 0 && cyr(probe) >= lat(probe)) ? 'ru' : 'en-US';
  const boxes = await recognizeBoxes(png, lang).catch(() => ({ lines: [] }));
  const cx = relX * scale, cy = relY * scale;
  let best = null, bestDist = Infinity;
  for (const line of boxes.lines || []) {
    for (const w of line.words || []) {
      if (cx >= w.x && cx <= w.x + w.w && cy >= w.y && cy <= w.y + w.h) return String(w.text).trim();
      const d = Math.abs(w.x + w.w / 2 - cx) + Math.abs(w.y + w.h / 2 - cy);
      if (d < bestDist) { bestDist = d; best = w.text; }
    }
  }
  return best ? String(best).trim() : probe;
}

module.exports = { recognize, recognizeBoxes, recognizeForSelection, recognizeWordAt };
