'use strict';
// OCR via Windows built-in Windows.Media.Ocr (PowerShell bridge). Zero deps, offline,
// supports all languages installed in Windows (incl. Russian + English).
const { app } = require('electron');
const { execFile } = require('child_process');
const fs = require('fs');
const path = require('path');

const SCRIPT = path.join(__dirname, 'ocr.ps1');

// Accepts a PNG buffer, returns recognized text (string, may be empty).
async function recognize(pngBuffer) {
  const tmp = path.join(app.getPath('temp'), `shotik-ocr-${Date.now()}.png`);
  fs.writeFileSync(tmp, pngBuffer);
  try {
    return await new Promise((resolve, reject) => {
      execFile(
        'powershell.exe',
        ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', SCRIPT, tmp],
        { timeout: 25000, windowsHide: true, encoding: 'buffer', maxBuffer: 4 * 1024 * 1024 },
        (err, stdout, stderr) => {
          if (err) return reject(new Error('OCR failed: ' + (stderr ? stderr.toString('utf8') : err.message)));
          resolve(stdout.toString('utf8').replace(/\r/g, '').trim());
        }
      );
    });
  } finally {
    try { fs.unlinkSync(tmp); } catch (_) {}
  }
}

module.exports = { recognize };
