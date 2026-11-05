'use strict';
// A real update of the installed app on the real profile, through GitHub: the path a user takes. It quits the installed
// app and starts it again, so a fresh check finds the release, and presses the real button on the real toast. It takes
// over the desktop and changes what is installed, so it needs the owner's agreement and an idle machine.
//   node test/live/update.js --expect 2.1.1 [--min-idle 120]
const { spawn, execFileSync } = require('child_process');
const crypto = require('crypto');
const { Live, UserBack, trayIcon, until, sleep, VK, SCRATCH, fs, path, os } = require('./lib');

const argOf = (n, d) => { const i = process.argv.indexOf('--' + n); return i === -1 ? d : process.argv[i + 1]; };
const EXPECT = argOf('expect');
if (!/^\d+\.\d+\.\d+$/.test(EXPECT || '')) throw new Error('usage: node test/live/update.js --expect 2.1.1');
const L = new Live({ minIdleSec: Number(argOf('min-idle', 120)) });
const call = (op, a) => L.probe.call(op, a);
const asList = (v) => (Array.isArray(v) ? v : v ? [v] : []);
const DIR = path.join(process.env.LOCALAPPDATA, 'Programs', 'shotik');
const EXE = path.join(DIR, 'Shotik.exe');
const PROFILE = path.join(process.env.APPDATA, 'Shotik');
const UNINSTALL = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\dev.shotik.app';
const RUN = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run';
// The toast's button, in DIP from the toast's corner: the middle of it, in ui/toast.slint.
const TOAST = { w: 380, h: 92, buttonX: 150, buttonY: 69 };

function ps(script) {
  return execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script],
    { encoding: 'utf8', windowsHide: true, timeout: 30000 }).trim();
}

const installedPid = () => Number(ps(`(Get-Process Shotik -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq '${EXE.replace(/'/g, "''")}' } | Select-Object -First 1).Id`));
const sha256 = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');

/** Hashes only: settings.json may hold a DeepL key, and nothing of it should reach a log. */
function installState() {
  const file = (name) => {
    const p = path.join(PROFILE, name);
    return fs.existsSync(p) ? sha256(p) : null;
  };
  return {
    settings: file('settings.json'),
    history: file('history.json'),
    autostart: ps(`(Get-ItemProperty '${RUN}').'dev.shotik.app'`),
    shortcuts: Number(ps(`@(Get-ChildItem ([Environment]::GetFolderPath('Programs')) -Filter 'Shotik*.lnk').Count`)),
    desktop: fs.existsSync(path.join(ps(`[Environment]::GetFolderPath('Desktop')`), 'Shotik.lnk')),
    uninstaller: fs.existsSync(path.join(DIR, 'Uninstall Shotik.exe')),
  };
}

const version = () => ps(`(Get-ItemProperty '${UNINSTALL}').DisplayVersion`);

(async () => {
  let mine;
  const shots = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-live-update-'));
  try {
    await L.begin();
    const from = version();
    L.note(`installed ${from}, expecting ${EXPECT}`);
    if (!L.check('an older version is installed', from !== EXPECT, from)) return;
    const pid = installedPid();
    if (!L.check('the installed app is running', pid > 0, String(pid))) return;
    const before = installState();

    // Quit the way a person does, so the next start checks within a minute.
    const list = async (p) => asList((await call('windows', { pid: p })).windows).filter((w) => w.cls !== 'Winit Thread Event Target');
    const tray = trayIcon(L, pid);
    const icon = await tray.reveal();
    if (!L.check('the shell shows the tray icon', icon.found, JSON.stringify(icon))) return;
    await tray.rightClick(icon);
    const menu = await until(async () => (await list(pid)).find((x) => x.cls === '#32768'), 2500, 80);
    if (!L.check('a real right click opens the menu', !!menu)) return;
    await sleep(250);
    await L.key(pid, VK.up);
    await L.key(pid, VK.enter);
    if (!L.check('its Exit item quits the app', await until(async () => installedPid() !== pid, 8000, 200))) return;

    // Something for the toast to steal the keyboard from.
    const monitors = asList((await call('monitors')).monitors);
    const side = monitors.find((m) => !m.primary) || monitors[0];
    mine = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', path.join(SCRATCH, 'testwin.ps1'), '-X', String(side.x + 200), '-Y', String(side.y + 200), '-W', '800', '-H', '400'], { stdio: 'ignore', windowsHide: true });
    const form = await until(async () => asList((await call('windows', { pid: mine.pid })).windows)[0], 8000, 100);
    await sleep(300);
    await L.click([mine.pid], form.x + form.w - 60, form.y + form.h - 40);
    await sleep(300);
    const focused = await call('fg');

    const app = spawn(EXE, ['--hidden'], { stdio: 'ignore', detached: true });
    app.unref();
    const running = await until(async () => installedPid(), 8000, 200);
    L.check('it starts again', running > 0, String(running));
    L.note('waiting for the check, a minute after starting');
    const scale = (w) => w.w / TOAST.w;
    const isToast = (w) => w.w >= TOAST.w && w.h >= TOAST.h && Math.abs(w.w / w.h - TOAST.w / TOAST.h) < 0.05;
    const started = Date.now();
    let toast;
    while (!toast && Date.now() - started < 150000) {
      await L.guard();
      toast = (await list(running)).find(isToast);
      if (!toast) await sleep(500);
    }
    if (!L.check('a toast offers the update', !!toast, `after ${Math.round((Date.now() - started) / 1000)} s`)) return;
    await sleep(400);
    L.check('  without taking the keyboard', (await call('fg')).pid === focused.pid);
    await call('shot', { x: toast.x, y: toast.y, w: toast.w, h: toast.h, scale: 1, path: path.join(shots, 'offer.png') });
    const offeredAt = Date.now();

    await L.click([running], toast.x + Math.round(TOAST.buttonX * scale(toast)), toast.y + Math.round(TOAST.buttonY * scale(toast)));
    L.note('pressed the button');
    const left = await until(async () => installedPid() !== running, 60000, 200);
    if (!L.check('the app leaves by itself', left, `${Date.now() - offeredAt} ms`)) return;
    const back = await until(async () => { const p = installedPid(); return p > 0 && p !== running ? p : 0; }, 90000, 200);
    if (!L.check('the installer brings it back', back > 0, `${Date.now() - offeredAt} ms after the click`)) return;
    L.check('the new version is installed', version() === EXPECT, version());
    const done = await until(async () => (await list(back)).find(isToast), 10000, 100);
    if (L.check('a toast says it was updated', !!done)) {
      await sleep(300);
      await call('shot', { x: done.x, y: done.y, w: done.w, h: done.h, scale: 1, path: path.join(shots, 'done.png') });
    }
    L.check('  and has not taken the keyboard', (await call('fg')).pid === focused.pid);
    L.check('still in the tray only: no main window opened', !(await list(back)).some((w) => w.w > 800 && w.h > 500));
    L.check('no working copy left in the install folder', !fs.existsSync(EXE + '.new'));

    const after = installState();
    for (const key of Object.keys(before)) L.check(`${key} is as it was`, before[key] === after[key], String(after[key]).slice(0, 16));

    L.note('waiting for the new version to clean up after the installer');
    const cleaned = await until(async () => {
      await L.guard();
      return !fs.readdirSync(os.tmpdir()).some((name) => name.startsWith('shotik-update-'));
    }, 90000, 1000);
    L.check('the downloaded installer is gone a minute later', cleaned);
    if (L.summary()) process.exitCode = 2;
  } catch (e) {
    if (e instanceof UserBack) { console.log('USER RETURNED, STOPPED: ' + e.message); process.exitCode = 3; } else { console.log('FAILED: ' + e.stack); process.exitCode = 1; }
  } finally {
    if (mine) { try { execFileSync('taskkill', ['/F', '/T', '/PID', String(mine.pid)], { stdio: 'ignore', windowsHide: true }); } catch (_) { /* already gone */ } }
    L.probe.close();
    console.log('kept for review, delete after: ' + shots);
  }
})();
