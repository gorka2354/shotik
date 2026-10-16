// Launch the installed app without inheriting an interactive terminal's handles.
// Quit any existing Shotik instance from the tray before running this script.
const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');

if (process.platform !== 'win32') {
  console.error('This launcher requires Windows and an installed copy of Shotik.');
  process.exit(1);
}

const exe = path.join(process.env.LOCALAPPDATA, 'Programs', 'shotik', 'Shotik.exe');
const dir = path.join(process.env.LOCALAPPDATA, 'Shotik', 'logs');
fs.mkdirSync(dir, { recursive: true });
const stamp = new Date().toISOString().replace(/[:.]/g, '-');
const log = name => path.join(dir, stamp + '-' + name + '.log');
const out = fs.openSync(log('stdout'), 'a');
const err = fs.openSync(log('stderr'), 'a');
const child = spawn(exe, ['--hidden', '--enable-logging=file', '--log-file=' + log('chromium')], {
  cwd: path.dirname(exe), detached: true, windowsHide: true,
  stdio: ['ignore', out, err],
});
child.once('error', e => { console.error(e.message); process.exitCode = 1; });
child.once('spawn', () => {
  console.log(JSON.stringify({ pid: child.pid, logDirectory: dir, logPrefix: stamp }));
  child.unref();
});
fs.closeSync(out);
fs.closeSync(err);
