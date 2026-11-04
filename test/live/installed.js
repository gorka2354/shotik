'use strict';
// The installed app, on the user's real profile: its real hotkeys and its real tray icon. It only opens windows and
// closes them again, so nothing is saved, copied or translated.
//   node test/live/installed.js [--min-idle 120]
const { spawn, spawnSync, execFileSync } = require('child_process');
const { Live, UserBack, until, sleep, VK, SCRATCH, fs, path, os } = require('./lib');

const argOf = (n, d) => { const i = process.argv.indexOf('--' + n); return i === -1 ? d : process.argv[i + 1]; };
const L = new Live({ minIdleSec: Number(argOf('min-idle', 120)) });
const call = (op, a) => L.probe.call(op, a);
const asList = (v) => (Array.isArray(v) ? v : v ? [v] : []);
const EXE = path.join(process.env.LOCALAPPDATA, 'Programs', 'shotik', 'Shotik.exe');
const SNAPSHOT = 0x2C;

function installedPid() {
  const out = execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
    `(Get-Process Shotik -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq '${EXE.replace(/'/g, "''")}' } | Select-Object -First 1).Id`], { encoding: 'utf8', windowsHide: true });
  return Number(out.trim());
}

// Where the shell draws the icon, whether on the taskbar or in the flyout of hidden icons.
function findIcon() {
  const out = execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', path.join(SCRATCH, 'tray-icon.ps1')], { encoding: 'utf8', windowsHide: true });
  return JSON.parse(out.trim().split(/\r?\n/).pop());
}

(async () => {
  let mine;
  const shots = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-live-installed-'));
  try {
    await L.begin();
    const pid = installedPid();
    if (!L.check('the installed app is running', pid > 0, String(pid))) return;
    const list = async () => asList((await call('windows', { pid }) ).windows).filter((w) => w.cls !== 'Winit Thread Event Target');
    const monitors = asList((await call('monitors')).monitors);
    const side = monitors.find((m) => !m.primary) || monitors[0];
    mine = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', path.join(SCRATCH, 'testwin.ps1'), '-X', String(side.x + 200), '-Y', String(side.y + 200), '-W', '800', '-H', '400'], { stdio: 'ignore', windowsHide: true });
    const form = await until(async () => asList((await call('windows', { pid: mine.pid })).windows)[0], 8000, 100);
    await sleep(300);
    await L.click([mine.pid], form.x + form.w - 60, form.y + form.h - 40);
    await sleep(300);
    L.prior = await call('fg');
    L.check('no window of its own while in the tray', (await list()).length === 0);

    const primary = monitors.find((m) => m.primary);
    await L.move(primary.x + 1000, primary.y + 1000);
    await sleep(60);
    const t0 = Date.now();
    await L.chord(SNAPSHOT);
    const opened = await until(async () => {
      const wins = await list();
      return wins.filter((w) => monitors.some((m) => w.x === m.x && w.y === m.y && w.w === m.w && w.h === m.h)).length === monitors.length;
    }, 3000, 15);
    const ms = Date.now() - t0;
    L.check('PrintScreen opens the overlay on every monitor', opened, `${ms} ms`);
    if (opened) {
      await sleep(100);
      const fg = await call('fg');
      L.check('  with the keyboard on the monitor under the cursor', fg.pid === pid && fg.x === primary.x && fg.y === primary.y, `${fg.x},${fg.y}`);
      await L.key(pid, VK.esc);
      const closed = await until(async () => (await list()).length === 0, 1500, 30);
      L.check('  Esc closes it, nothing saved', closed);
      await sleep(150);
      L.check('  and focus comes back', (await call('fg')).pid === mine.pid);
    }

    // A real click on the real tray icon.
    const t = await call('tray', { cls: 'Shotik.NativeTray', pid });
    if (L.check('the tray icon exists', t.found && t.hr === 0, JSON.stringify(t))) {
      const realClick = async (x, y, down, up) => {
        await L.move(x, y);
        await sleep(300);
        await L.guard();
        await L.assertCursor(x, y);
        L.mark(await call('btn', { flags: down }));
        await sleep(60);
        L.mark(await call('btn', { flags: up }));
      };
      // Windows hides a new icon in the flyout, and the rectangle it reports is then the arrow's. The arrow
      // moves as other icons come and go, so the rectangle is read right before the click.
      const openFlyout = async () => {
        let found = findIcon();
        if (found.found) return found;
        const arrow = await call('tray', { cls: 'Shotik.NativeTray', pid });
        await realClick(arrow.x + Math.floor(arrow.w / 2), arrow.y + Math.floor(arrow.h / 2), 0x0002, 0x0004);
        await until(async () => /Overflow/.test((await call('fg')).cls), 2000, 50);
        await sleep(250);
        found = findIcon();
        return found;
      };
      const icon = await openFlyout();
      L.note('icon ' + JSON.stringify(icon));
      if (L.check('the shell shows it', icon.found)) {
        await realClick(icon.x, icon.y, 0x0002, 0x0004);
        const main = await until(async () => (await list()).find((x) => x.w > 800 && x.h > 500), 2500, 60);
        if (L.check('a real click opens the main window', !!main)) {
          await sleep(500);
          const fg = await call('fg');
          L.check('  with the keyboard', fg.pid === pid, fg.cls);
          await L.key(pid, VK.alt, VK.f4);
          await until(async () => (await list()).length === 0, 1500, 40);
          await sleep(200);
        }
        const again = await openFlyout();
        if (L.check('the shell shows it again', again.found)) {
          await realClick(again.x, again.y, 0x0008, 0x0010);
          const menu = await until(async () => (await list()).find((x) => x.cls === '#32768'), 2500, 60);
          if (L.check('a real right click opens the menu', !!menu)) {
            await sleep(200);
            await call('shot', { x: menu.x - 10, y: menu.y - 10, w: menu.w + 20, h: menu.h + 20, scale: 1, path: path.join(shots, 'tray-menu.png') });
            const fg = await call('fg');
            L.check('  and it has the keyboard', fg.pid === pid, fg.cls);
            await L.key(pid, VK.esc);
            await until(async () => !(await list()).find((x) => x.cls === '#32768'), 1000, 40);
          }
        }
        await sleep(200);
      }
    }

    // What the Start menu's shortcuts do.
    await L.click([mine.pid], form.x + form.w - 60, form.y + form.h - 40);
    await sleep(300);
    const second = spawnSync(EXE, ['--capture', 'region'], { timeout: 10000, windowsHide: true });
    L.check('a second start forwards --capture and exits', second.status === 0, `status ${second.status}`);
    const forwarded = await until(async () => (await list()).length >= monitors.length, 3000, 30);
    L.check('  and the running app opens the overlay', forwarded);
    if (forwarded) {
      await sleep(150);
      await L.key(pid, VK.esc);
      await until(async () => (await list()).length === 0, 1500, 30);
    }
    L.check('still one instance', installedPid() === pid);
    if (L.summary()) process.exitCode = 2;
  } catch (e) {
    if (e instanceof UserBack) { console.log('USER RETURNED, STOPPED: ' + e.message); process.exitCode = 3; } else { console.log('FAILED: ' + e.stack); process.exitCode = 1; }
  } finally {
    if (mine) { try { execFileSync('taskkill', ['/F', '/T', '/PID', String(mine.pid)], { stdio: 'ignore', windowsHide: true }); } catch (_) {} }
    L.probe.close();
    console.log('kept for review, delete after: ' + shots);
  }
})();
