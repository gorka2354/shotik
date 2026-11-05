'use strict';
// The production path, no test routes: a private profile, a real UI Automation watcher, a real translation.
const { spawn, execFileSync } = require('child_process');
const { Live, UserBack, trayIcon, until, sleep, VK, ROOT, SCRATCH, os, fs, path } = require('./lib');
const { preserveClipboard } = require(path.join(ROOT, 'test/ghost/clipboard'));

const EXE = process.argv[2];
const L = new Live({ minIdleSec: Number(process.argv[3] || 60) });
const call = (op, a) => L.probe.call(op, a);
const asList = (v) => (Array.isArray(v) ? v : v ? [v] : []);
const hk = (vk) => [VK.ctrl, VK.alt, VK.shift, vk];
const HOTKEYS = { region: 'Ctrl+Alt+Shift+F9', fullscreen: 'Ctrl+Alt+Shift+F10', repeatLast: 'Ctrl+Alt+Shift+F11', textGrab: 'Ctrl+Alt+Shift+F8', record: 'Ctrl+Alt+Shift+F7', recordAudio: 'Ctrl+Alt+Shift+F6', translateSelection: 'Ctrl+Alt+Shift+F5' };

(async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-live-sel-'));
  const appData = path.join(dir, 'appdata'), temp = path.join(dir, 'temp'), saved = path.join(dir, 'saved');
  const shots = path.join(dir, 'shots');
  for (const d of [path.join(appData, 'Shotik'), temp, saved, shots]) fs.mkdirSync(d, { recursive: true });
  fs.writeFileSync(path.join(appData, 'Shotik', 'settings.json'), JSON.stringify({ saveDir: saved, hotkeys: HOTKEYS, mcp: { port: 7699 }, language: 'ru' }));
  fs.writeFileSync(path.join(appData, 'Shotik', 'history.json'), '[]');
  let app, mine, clip;
  const log = () => { try { return fs.readFileSync(path.join(temp, 'shotik-bubble.log'), 'utf8').split(String.fromCharCode(10)).filter(Boolean).slice(-12); } catch { return []; } };
  const list = async () => asList((await call('windows', { pid: app.pid })).windows).filter((w) => w.cls !== 'Winit Thread Event Target');
  const human = async (x, y, times) => {
    await L.assertTarget([mine.pid], x, y);
    await L.move(x, y);
    await sleep(80);
    for (let i = 0; i < times; i++) {
      await L.guard();
      L.mark(await call('btn', { flags: 0x0002 }));
      await sleep(95);
      L.mark(await call('btn', { flags: 0x0004 }));
      await sleep(110);
    }
  };
  try {
    await L.begin();
    const monitors = asList((await call('monitors')).monitors);
    clip = await preserveClipboard();
    const env = { ...process.env, APPDATA: appData, TEMP: temp, TMP: temp };
    for (const k of ['SHOTIK_TEST', 'SHOTIK_GHOST', 'SHOTIK_FAKE_SCREEN', 'SHOTIK_TRANSLATE_STUB', 'SHOTIK_PORT', 'SHOTIK_SAVE_DIR', 'SHOTIK_REAL_CAPTURE']) delete env[k];
    app = spawn(EXE, ['--hidden'], { env, stdio: 'ignore', windowsHide: true });
    await sleep(1500);
    const m = monitors.find((x) => !x.primary);
    const script = path.join(SCRATCH, 'testwin-wpf.ps1');
    mine = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', script, '-X', String(m.x + 200), '-Y', String(m.y + 200), '-W', '900', '-H', '400'], { stdio: 'ignore' });
    const form = await until(async () => asList((await call('windows', { pid: mine.pid })).windows).find((w) => w.w > 300), 15000, 100);
    if (!L.check('a window with a real TextPattern', !!form)) return;
    await sleep(500);
    await L.click([mine.pid], form.x + form.w - 60, form.y + form.h - 40);
    await sleep(300);
    L.prior = await call('fg');
    const before = await call('clip');
    const line1 = { x: form.x + 150, y: form.y + 55 };
    await human(line1.x, line1.y, 2);
    const bubble = await until(async () => (await list()).find((b) => b.w <= 140 && b.h <= 140), 4000, 60);
    if (L.check('a real double click brings the bubble through UI Automation', !!bubble, bubble ? `${bubble.w}x${bubble.h} at ${bubble.x},${bubble.y}` : 'diag: ' + log().join(' | '))) {
      await call('shot', { x: bubble.x - 80, y: bubble.y - 80, w: bubble.w + 160, h: bubble.h + 160, scale: 1, path: path.join(shots, 'bubble.png') });
      L.check('  the app with the selection keeps the focus', (await call('fg')).pid === mine.pid);
      L.check('  the bubble is topmost', bubble.topmost);
      await L.click([app.pid], bubble.x + Math.floor(bubble.w / 2), bubble.y + Math.floor(bubble.h / 2));
      const popup = await until(async () => (await list()).find((p) => p.w > 200 && p.h > 100), 5000, 60);
      if (L.check('  its click opens the translation', !!popup, popup ? `${popup.w}x${popup.h} at ${popup.x},${popup.y}` : '')) {
        await sleep(2500);
        await call('shot', { x: popup.x - 20, y: popup.y - 20, w: popup.w + 40, h: popup.h + 40, scale: 1, path: path.join(shots, 'popup.png') });
        L.note('diag: ' + log().join(' | '));
      }
      const after = await call('clip');
      L.check('  UI Automation left the clipboard alone', after.formats === before.formats && after.text === before.text, after.formats);
      const closeBox = popup && { x: popup.x + popup.w - 30, y: popup.y + 30 };
      if (closeBox) { await L.click([app.pid], closeBox.x, closeBox.y); await sleep(400); }
      await sleep(900);
      L.note('windows after closing the popup: ' + JSON.stringify((await list()).map((x) => [x.w, x.h])));
      L.note('diag after: ' + log().slice(-4).join(' | '));
    }
    // The translate hotkey, with the keys held as a person holds them.
    await L.click([mine.pid], form.x + form.w - 60, form.y + form.h - 40);
    await sleep(400);
    await human(form.x + 260, form.y + 85, 2);
    await sleep(500);
    await L.guard();
    const chord = hk(VK.f5);
    L.mark(await call('keydown', { vks: chord }));
    await sleep(280);
    L.mark(await call('keyup', { vks: [...chord].reverse() }));
    const byKey = await until(async () => (await list()).find((p) => p.w > 200 && p.h > 100), 6000, 60);
    L.check('the translate hotkey held like a person holds it reads the selection', !!byKey, byKey ? `${byKey.w}x${byKey.h}` : 'diag: ' + log().join(' | '));
    await sleep(2500);
    if (byKey) await call('shot', { x: byKey.x - 20, y: byKey.y - 20, w: byKey.w + 40, h: byKey.h + 40, scale: 1, path: path.join(shots, 'popup-hotkey.png') });
    const end_ = await call('clip');
    L.check('  the clipboard text is back as it was', end_.text === before.text, `${end_.formats} was ${before.formats}`);
    if (byKey) { await L.click([app.pid], byKey.x + byKey.w - 30, byKey.y + 30); await sleep(400); }
    execFileSync('taskkill', ['/F', '/T', '/PID', String(mine.pid)], { stdio: 'ignore', windowsHide: true });
    await sleep(300);
    // Text an application draws itself: no TextPattern, so the highlight is read by OCR.
    mine = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', path.join(SCRATCH, 'testwin.ps1'), '-X', String(m.x + 200), '-Y', String(m.y + 200), '-W', '900', '-H', '400', '-FontSize', '13'], { stdio: 'ignore', windowsHide: true });
    const plain = await until(async () => asList((await call('windows', { pid: mine.pid })).windows).find((w) => w.w > 300), 8000, 100);
    await sleep(500);
    await L.click([mine.pid], plain.x + plain.w - 60, plain.y + plain.h - 40);
    await sleep(400);
    await human(plain.x + 90, plain.y + 50, 2);
    const ocrBubble = await until(async () => (await list()).find((b) => b.w <= 140 && b.h <= 140), 5000, 60);
    L.check('without a TextPattern, the highlight is read by OCR and brings the bubble', !!ocrBubble, 'diag: ' + log().slice(-3).join(' | '));
    if (ocrBubble) await call('shot', { x: plain.x, y: plain.y, w: plain.w, h: 200, scale: 1, path: path.join(shots, 'ocr-gesture.png') });
    // Quit the way a person does: a real right click on the icon, the menu's last item.
    const tray = trayIcon(L, app.pid);
    const icon = await tray.reveal();
    if (L.check('the shell shows the tray icon', icon.found, JSON.stringify(icon))) {
      await tray.rightClick(icon);
      const menu = await until(async () => (await list()).find((x) => x.cls === '#32768'), 2500, 80);
      if (L.check('a real right click opens the menu', !!menu)) {
        await sleep(250);
        await L.key(app.pid, VK.up);
        await L.key(app.pid, VK.enter);
        const quit = await until(async () => app.exitCode !== null, 5000, 100);
        L.check('its Exit item quits the app', quit, `exit code ${app.exitCode}`);
        await sleep(200);
        const after = await call('tray', { cls: 'Shotik.NativeTray', pid: app.pid });
        L.check('  and takes the tray icon with it', !after.found);
      }
    }
    const failed = L.summary();
    if (failed) process.exitCode = 2;
  } catch (e) {
    if (e instanceof UserBack) { console.log('USER RETURNED: ' + e.message); process.exitCode = 3; } else { console.log('FAILED: ' + e.stack); process.exitCode = 1; }
  } finally {
    if (app && app.exitCode === null) { try { execFileSync('taskkill', ['/F', '/T', '/PID', String(app.pid)], { stdio: 'ignore', windowsHide: true }); } catch (_) {} }
    if (mine) { try { execFileSync('taskkill', ['/F', '/T', '/PID', String(mine.pid)], { stdio: 'ignore', windowsHide: true }); } catch (_) {} }
    if (clip) { try { await clip.restore(); } catch (e) { console.log('CLIPBOARD NOT RESTORED: ' + e.message); } }
    L.probe.close();
    await sleep(300);
    for (const n of ['appdata', 'temp', 'saved']) fs.rmSync(path.join(dir, n), { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    console.log('kept for review, delete after: ' + shots);
  }
})();
