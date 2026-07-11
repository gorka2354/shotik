'use strict';
// OCR-for-selection correctness. Renders the ocr-cases.json phrases to small
// PNGs (Telegram-post-sized text), OCRs each via recognizeForSelection, and
// checks the text comes back faithfully. This is the guard the user asked for:
// "important to test very well that it returns the correct text".
const { execFileSync } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');
const { makeApp } = require('./harness');
const { test, assert } = require('../tiny');

const PORT = 7496;
const CASES = require('../ocr-cases.json');

const norm = (s) => String(s).toLowerCase().replace(/[^\p{L}\p{N}]+/gu, ' ').trim();
// tolerant compare: normalized equality, or ≥90% of ground-truth words present
function faithful(got, want) {
  const g = norm(got), w = norm(want);
  if (g === w) return true;
  const gw = new Set(g.split(' '));
  const words = w.split(' ').filter(Boolean);
  const hits = words.filter((x) => gw.has(x)).length;
  return hits / words.length >= 0.9;
}

test('OCR-for-selection reads small EN/RU text faithfully', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-ocrfx-'));
  execFileSync('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass',
    '-File', path.join(__dirname, 'ocr-fixtures.ps1'), '-OutDir', dir], { stdio: 'ignore' });

  const app = makeApp({ port: PORT });
  const failures = [];
  try {
    await app.waitReady();
    for (const c of CASES) {
      const r = await app.post('ocr-selection', { path: path.join(dir, c.n + '.png') });
      const got = (r && r.text) || '';
      if (!faithful(got, c.t)) failures.push(`${c.n}: want "${c.t}" got "${got}"`);
    }
  } finally {
    await app.stop();
    try { fs.rmSync(dir, { recursive: true, force: true }); } catch (_) {}
  }
  assert(failures.length === 0, 'OCR mismatches:\n      ' + failures.join('\n      '));
});

// End-to-end wiring of the OCR fallback: a fixture "post" → OCR → bubble carries
// the text → clicking translates it. (Real screen-region coordinates are
// smoke-tested live; here we feed a known image so the chain is deterministic.)
test('OCR fallback: fixture → bubble → click → translated popup', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-ocrfx2-'));
  execFileSync('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass',
    '-File', path.join(__dirname, 'ocr-fixtures.ps1'), '-OutDir', dir], { stdio: 'ignore' });
  const app = makeApp({ port: PORT });
  try {
    await app.waitReady();
    const g = await app.post('gesture-sim', { path: path.join(dir, 'en_white.png'), x: 900, y: 500 });
    assert(g.bubble === true, 'bubble shown from OCR text');
    assert(faithful(g.text, 'Good morning, please review the pull request'), 'OCR text is faithful, got: ' + g.text);
    const a = await app.post('bubble-activate', {});
    assert(a.popup === true, 'popup opened');
    const c = await app.post('exec-js', { target: 'tp', code: 'document.getElementById("orig").textContent' });
    assert(faithful(c, 'Good morning, please review the pull request'), 'popup shows the OCR text, got: ' + c);
  } finally {
    await app.stop();
    try { fs.rmSync(dir, { recursive: true, force: true }); } catch (_) {}
  }
});

module.exports = {};
