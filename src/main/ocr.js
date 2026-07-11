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
  if (cyr(ru) > 0 && cyr(ru) >= lat(ru)) return ru;         // the text really is Cyrillic
  const en = (await recognize(png, 'en-US').catch(() => '')).trim();
  return en || ru;                                           // clean Latin (or fall back)
}

// Returns { lines: [ { text, words: [ { text, x, y, w, h } ] } ] } in the
// pixel coordinates of the passed image. Windows only (Live Text mode);
// macOS falls back to a single line with no boxes.
async function recognizeBoxes(pngBuffer) {
  const tmp = path.join(app.getPath('temp'), `shotik-ocrb-${Date.now()}.png`);
  fs.writeFileSync(tmp, pngBuffer);
  try {
    if (process.platform !== 'win32') {
      const text = await recognize(pngBuffer).catch(() => '');
      return { lines: text ? text.split('\n').map((t) => ({ text: t, words: [] })) : [] };
    }
    const out = await runPowerShell([tmp, '-Boxes']);
    if (!out) return { lines: [] };
    try { return JSON.parse(out); } catch (_) { return { lines: [] }; }
  } finally {
    try { fs.unlinkSync(tmp); } catch (_) {}
  }
}

module.exports = { recognize, recognizeBoxes, recognizeForSelection };
