'use strict';
const { spawn } = require('child_process');
const path = require('path');

// Keep clipboard objects in an STA process, never in a log or fixture file.
async function preserveClipboard() {
  const child = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-STA',
    '-ExecutionPolicy', 'Bypass', '-File', path.join(__dirname, 'clipboard.ps1')],
  { windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.resume();
  // An unexpectedly closed helper is reported by the exit/RESTORED checks,
  // rather than an unhandled EPIPE interrupting the rest of teardown.
  child.stdin.on('error', () => {});
  let output = '';
  child.stdout.setEncoding('utf8');
  const exited = new Promise((resolve) => child.once('exit', resolve));
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => { child.kill(); reject(new Error('clipboard snapshot timed out')); }, 15000);
    child.on('error', (e) => { clearTimeout(timer); reject(e); });
    child.stdout.on('data', (s) => {
      output += s;
      if (output.includes('READY')) { clearTimeout(timer); resolve(); }
    });
    child.once('exit', (code) => { clearTimeout(timer); reject(new Error('clipboard snapshot failed: ' + code)); });
  });
  return { async restore() {
    child.stdin.end('restore\n');
    const code = await exited;
    if (code !== 0 || !output.includes('RESTORED')) throw new Error('clipboard restore failed: ' + code);
  } };
}
module.exports = { preserveClipboard };
