'use strict';
// The live check on the real desktop: real windows, real hotkeys, real capture, real input.
//   node test/live/run.js <exe> [--only a,b,c] [--min-idle 120]
const { spawn, execFileSync, spawnSync } = require('child_process');
const { Live, UserBack, post, until, sleep, launchApp, stopApp, VK, digit, ROOT, SCRATCH, WHEEL, os, fs, path } = require('./lib');
const { preserveClipboard } = require(path.join(ROOT, 'test/ghost/clipboard'));

const EXE = process.argv[2];
const argOf = (n, d) => { const i = process.argv.indexOf('--' + n); return i === -1 ? d : process.argv[i + 1]; };
const ONLY = argOf('only', '') ? argOf('only', '').split(',') : null;
const PORT = 7698;

const HOTKEYS = { region: 'Ctrl+Alt+Shift+F9', fullscreen: 'Ctrl+Alt+Shift+F10', repeatLast: 'Ctrl+Alt+Shift+F11',
  textGrab: 'Ctrl+Alt+Shift+F8', record: 'Ctrl+Alt+Shift+F7', recordAudio: 'Ctrl+Alt+Shift+F6', translateSelection: 'Ctrl+Alt+Shift+F5' };
const hk = (vk) => [VK.ctrl, VK.alt, VK.shift, vk];

const L = new Live({ minIdleSec: Number(argOf('min-idle', 120)) });
const call = (op, args) => L.probe.call(op, args);
const P = (route, body, t) => post(PORT, route, body, t);
let app, monitors, shots, mine, formMonitor;

const cx = (m) => m.x + Math.floor(m.w / 2);
const cy = (m) => m.y + Math.floor(m.h / 2);
const windows = async () => (await call('windows', { pid: app.pid })).windows || [];
const asList = (v) => (Array.isArray(v) ? v : v ? [v] : []);
const list = async () => asList(await windows()).filter((w) => w.cls !== 'Winit Thread Event Target');

async function shot(name, rect, scale = 0.5) {
  const r = rect || { x: Math.min(...monitors.map((m) => m.x)), y: Math.min(...monitors.map((m) => m.y)),
    w: Math.max(...monitors.map((m) => m.x + m.w)) - Math.min(...monitors.map((m) => m.x)),
    h: Math.max(...monitors.map((m) => m.y + m.h)) - Math.min(...monitors.map((m) => m.y)) };
  const file = path.join(shots, name + '.png');
  await call('shot', { x: r.x, y: r.y, w: r.w, h: r.h, scale, path: file });
  return file;
}

async function openOverlay(m, vk = VK.f9) {
  await L.move(cx(m), cy(m));
  await sleep(50);
  const t0 = Date.now();
  await L.chord(...hk(vk));
  const ok = await until(async () => {
    const st = await P('state');
    if (!st.overlayOpen || st.overlayCount !== monitors.length) return false;
    const fg = await call('fg');
    return fg.pid === app.pid ? { st, fg } : false;
  }, 4000, 15);
  return { ok, ms: Date.now() - t0 };
}

async function closeOverlay() {
  try { await P('overlay-cancel'); } catch (_) {}
  await until(async () => !(await P('state')).overlayOpen, 1500);
}

async function overlayState(fgRect) {
  const st = await P('state');
  for (let i = 0; i < st.overlayCount; i++) {
    const s = await P('ui-state', { target: 'overlay', index: i });
    if (s.selection) return s;
  }
  return P('ui-state', { target: 'overlay', index: 0 });
}

const near = (a, b, t = 2) => Math.abs(a - b) <= t;

const sections = {
  async start() {
    const st = await P('state');
    L.check('hotkeys registered', asList(st.hotkeyErrors).length === 0, JSON.stringify(st.hotkeyErrors));
    L.check('no window on start with --hidden', (await list()).length === 0, (await list()).map((w) => w.cls + ' ' + w.w + 'x' + w.h).join(';'));
    const t = await call('tray', { cls: 'Shotik.NativeTray', pid: app.pid });
    L.check('tray icon present', t.found && t.hr === 0 && t.w > 0, JSON.stringify(t));
    L.note('foreground before the check: ' + L.prior.cls + ', layout ' + (L.prior.layout || '?'));
    L.note('monitors ' + monitors.map((m) => `${m.w}x${m.h}@${m.x},${m.y}${m.primary ? ' primary' : ''}`).join(' | '));
  },

  async overlay_basic() {
    for (const [i, m] of monitors.entries()) {
      const { ok, ms } = await openOverlay(m);
      if (!L.check(`overlay opens by hotkey with the cursor on monitor ${i}`, ok, `${ms} ms`)) continue;
      const wins = await list();
      for (const [j, other] of monitors.entries()) {
        const w = wins.find((x) => x.x === other.x && x.y === other.y && x.w === other.w && x.h === other.h);
        L.check(`  covers monitor ${j} exactly and topmost`, w && w.topmost, w ? `dpi ${w.dpi}` : JSON.stringify(wins.map((x) => [x.x, x.y, x.w, x.h])));
      }
      const fg = await call('fg');
      L.check('  keyboard focus is on the monitor under the cursor', fg.pid === app.pid && fg.x === m.x && fg.y === m.y && fg.w === m.w, `${fg.x},${fg.y} ${fg.w}x${fg.h}`);
      const below = await call('at', { x: cx(m), y: m.y + m.h - 4 });
      L.check('  the taskbar edge is covered', below.pid === app.pid, below.cls);
      L.note('marks ' + (await P('overlay-timing')).join(' '));
      if (i === 0) await shot('overlay-all-monitors', null, 0.33);
      await L.key(app.pid, VK.esc);
      const closed = await until(async () => !(await P('state')).overlayOpen, 1500);
      L.check('  Esc closes it', closed);
      await sleep(150);
      const back = await call('fg');
      L.check('  focus returns to the previous window', back.pid === L.prior.pid, back.cls);
    }
  },

  async copy_each() {
    for (const [i, m] of monitors.entries()) {
      const { ok } = await openOverlay(m);
      if (!L.check(`overlay for a copy on monitor ${i}`, ok)) continue;
      await L.drag([app.pid], m.x + 200, m.y + 200, m.x + 800, m.y + 600);
      const s = await overlayState();
      L.check('  a real drag makes the selection', s.selection && near(s.selection.w, 600, 3) && near(s.selection.h, 400, 3) && s.toolbarVisible, JSON.stringify(s.selection));
      if (i === 0) await shot('selection-toolbar', { x: m.x + 100, y: m.y + 100, w: 1000, h: 750 }, 1);
      await L.key(app.pid, VK.enter);
      await until(async () => !(await P('state')).overlayOpen, 2000);
      await sleep(250);
      const clip = await call('clip');
      L.check('  the clipboard has the image at its physical size', clip.hasImage && near(clip.w, 600, 3) && near(clip.h, 400, 3), `${clip.w}x${clip.h} formats ${clip.formats}`);
      L.check('  plain copy holds no text', clip.text === null, String(clip.text).slice(0, 30));
      const hist = await P('history-list', { limit: 1 });
      const item = asList(hist)[0];
      const file = item && (item.path || item.file);
      L.check('  saved to the folder', file && fs.existsSync(file), file ? path.basename(file) : JSON.stringify(item));
      const fg = await call('fg');
      L.check('  focus returns after the copy', fg.pid === L.prior.pid, fg.cls);
      const t = await P('toast-visible');
      const toast = (await list()).find((w) => w.w < 900 && w.h < 300);
      L.check('  toast visible and it took no focus', t.visible && toast && toast.topmost, toast ? `${toast.w}x${toast.h} at ${toast.x},${toast.y} noactivate=${toast.noactivate}` : 'none');
      if (i === 0 && toast) await shot('toast', { x: toast.x - 30, y: toast.y - 30, w: toast.w + 60, h: toast.h + 60 }, 1);
    }
  },

  async tools() {
    const m = monitors.find((x) => x.primary);
    const { ok } = await openOverlay(m);
    if (!L.check('overlay for annotations', ok)) return;
    await L.drag([app.pid], m.x + 300, m.y + 300, m.x + 1500, m.y + 1100);
    await L.key(app.pid, digit(2));
    let s = await overlayState();
    L.check('key 2 selects the pen', s.tool === 'pen', s.tool);
    await L.drag([app.pid], m.x + 500, m.y + 500, m.x + 900, m.y + 800, { steps: 20 });
    s = await overlayState();
    L.check('a real stroke adds an annotation', s.annotationCount === 1, String(s.annotationCount));
    await L.key(app.pid, VK.ctrl, VK.z);
    s = await overlayState();
    L.check('Ctrl+Z undoes it', s.annotationCount === 0);
    await L.key(app.pid, VK.ctrl, VK.y);
    s = await overlayState();
    L.check('Ctrl+Y redoes it', s.annotationCount === 1);
    await L.key(app.pid, digit(9));
    await L.click([app.pid], m.x + 1000, m.y + 500);
    await sleep(100);
    s = await overlayState();
    L.check('the text tool opens an editor on a click', s.editorOpen === true);
    await L.type(app.pid, 'Привет мир 123');
    await sleep(120);
    s = await overlayState();
    L.check('Cyrillic reaches the editor', s.editorText === 'Привет мир 123', s.editorText);
    await L.click([app.pid], m.x + 1300, m.y + 1000);
    await sleep(120);
    s = await overlayState();
    L.check('clicking away commits the text', s.annotationCount === 2 && !s.editorOpen, String(s.annotationCount));
    await shot('annotations', { x: m.x + 250, y: m.y + 250, w: 1300, h: 1000 }, 0.6);
    await L.key(app.pid, digit(1));
    const before = (await overlayState()).selection;
    await L.key(app.pid, VK.right);
    s = await overlayState();
    L.check('an arrow key nudges the selection', s.selection.x > before.x, `${before.x} -> ${s.selection.x}`);
    await L.key(app.pid, VK.alt);
    await sleep(200);
    const fg = await call('fg');
    L.check('Alt alone opens no menu', fg.pid === app.pid && fg.cls !== '#32768' && (await P('state')).overlayOpen, fg.cls);
    await L.key(app.pid, VK.alt, VK.f4);
    const closed = await until(async () => !(await P('state')).overlayOpen, 1500);
    L.check('Alt+F4 cancels the session and the app lives', closed && (await P('ping')) === 'pong');
  },

  async snap() {
    const w = (await asList((await call('windows', { pid: mine.pid })).windows))[0];
    if (!L.check('the check has its own window to snap to', !!w)) return;
    const m = monitors.find((x) => w.x >= x.x && w.x < x.x + x.w && w.y >= x.y && w.y < x.y + x.h);
    await closeOverlay();
    const { ok } = await openOverlay(m);
    if (!L.check('overlay for snapping', ok)) return;
    await L.move(w.x + 200, w.y + 200);
    await sleep(250);
    let s = await overlayState();
    const hover = s.hoverRect;
    L.note('hover ' + JSON.stringify(hover));
    await L.click([app.pid], w.x + 200, w.y + 200);
    await sleep(150);
    s = await overlayState();
    const sel = s.selection || {};
    const frame = w.frame || w;
    L.check('a click takes the window under the cursor', s.selection && near(sel.x + m.x, frame.x, 12) && near(sel.y + m.y, frame.y, 12) && near(sel.w, frame.w, 16) && near(sel.h, frame.h, 16), `${JSON.stringify(sel)} window ${frame.x - m.x},${frame.y - m.y} ${frame.w}x${frame.h}`);
    await L.key(app.pid, VK.esc);
    await sleep(120);
    await closeOverlay();
  },

  async snap_primary() {
    const m = monitors.find((x) => x.primary);
    if (m === formMonitor) return;
    const extra = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', path.join(SCRATCH, 'testwin.ps1'),
      '-X', String(m.x + 900), '-Y', String(m.y + 500), '-W', '1000', '-H', '600', '-Title', 'Shotik live check 2'], { stdio: 'ignore', windowsHide: true });
    try {
      const w = await until(async () => asList((await call('windows', { pid: extra.pid })).windows)[0], 8000, 100);
      if (!L.check('a second check window on the scaled monitor', !!w)) return;
      await sleep(300);
      const { ok } = await openOverlay(m);
      if (!L.check('overlay on the scaled monitor', ok)) return;
      await L.move(w.x + 200, w.y + 200);
      await sleep(250);
      await L.click([app.pid], w.x + 200, w.y + 200);
      await sleep(150);
      const s = await overlayState();
      const sel = s.selection || {};
      const frame = w.frame || w;
      L.check('a click takes that window at the right physical rectangle', s.selection && near(sel.x + m.x, frame.x, 12) && near(sel.y + m.y, frame.y, 12) && near(sel.w, frame.w, 16) && near(sel.h, frame.h, 16), `${JSON.stringify(sel)} window ${frame.x - m.x},${frame.y - m.y} ${frame.w}x${frame.h}`);
      await closeOverlay();
    } finally {
      try { execFileSync('taskkill', ['/F', '/T', '/PID', String(extra.pid)], { stdio: 'ignore', windowsHide: true }); } catch (_) {}
      await sleep(300);
    }
  },

  async save_dialog() {
    const m = monitors.find((x) => x.primary);
    const { ok } = await openOverlay(m);
    if (!L.check('overlay for Save as', ok)) return;
    await L.drag([app.pid], m.x + 400, m.y + 400, m.x + 900, m.y + 800);
    await L.key(app.pid, VK.ctrl, VK.s);
    const dialog = await until(async () => (await list()).find((w) => w.cls === '#32770'), 4000);
    if (L.check('Ctrl+S opens the native dialog', !!dialog, dialog ? `${dialog.w}x${dialog.h}` : '')) {
      await sleep(300);
      const above = await call('at', { x: dialog.x + Math.floor(dialog.w / 2), y: dialog.y + 12 });
      L.check('  the dialog is above the overlay', above.cls === '#32770', above.cls);
      await shot('save-dialog', null, 0.33);
      await L.key(app.pid, VK.esc);
      const gone = await until(async () => !(await list()).find((w) => w.cls === '#32770'), 2000);
      L.check('  Esc closes it', gone);
      const st = await P('state');
      L.note('overlay after cancelling the dialog: open=' + st.overlayOpen);
    }
    await closeOverlay();
  },

  async full_repeat() {
    const m = monitors.find((x) => !x.primary) || monitors[0];
    const { ok } = await openOverlay(m);
    if (!L.check('a known area to repeat later', ok)) return;
    await L.drag([app.pid], m.x + 150, m.y + 150, m.x + 750, m.y + 550);
    await L.key(app.pid, VK.enter);
    await until(async () => !(await P('state')).overlayOpen, 1500);
    await sleep(300);
    await L.move(cx(m), cy(m));
    await sleep(60);
    await L.chord(...hk(VK.f10));
    await sleep(900);
    let clip = await call('clip');
    L.check('the full-screen hotkey copies the monitor under the cursor', clip.hasImage && clip.w === m.w && clip.h === m.h, `${clip.w}x${clip.h} of ${m.w}x${m.h}`);
    L.check('  no overlay and focus stays', !(await P('state')).overlayOpen && (await call('fg')).pid === L.prior.pid);
    await L.chord(...hk(VK.f11));
    await sleep(900);
    clip = await call('clip');
    L.check('the repeat hotkey takes the last area', clip.hasImage && near(clip.w, 600, 3) && near(clip.h, 400, 3), `${clip.w}x${clip.h}`);
  },

  async pin() {
    const m = monitors.find((x) => x.primary);
    const { ok } = await openOverlay(m);
    if (!L.check('overlay for a pin', ok)) return;
    await L.drag([app.pid], m.x + 700, m.y + 500, m.x + 1300, m.y + 900);
    await L.key(app.pid, VK.p);
    await until(async () => !(await P('state')).overlayOpen, 1500);
    await sleep(300);
    const pins = await P('pins');
    const w = (await list()).find((x) => x.w > 300 && x.w < 900 && x.h > 200 && x.h < 700);
    L.check('P pins the shot where it was cut', asList(pins).length === 1 || pins.count === 1, JSON.stringify(pins).slice(0, 80));
    if (L.check('  the pin is a topmost window', w && w.topmost, w ? `${w.w}x${w.h} at ${w.x},${w.y}` : '')) {
      L.check('  at the same place', near(w.x, m.x + 700, 24) && near(w.y, m.y + 500, 24), `${w.x},${w.y}`);
      await L.wheel([app.pid], w.x + 100, w.y + 100, 2);
      await sleep(200);
      const z = (await list()).find((x) => x.hwnd === w.hwnd);
      L.check('  the wheel zooms it', z && z.w > w.w, z ? `${w.w} -> ${z.w}` : 'gone');
      await shot('pin', { x: z.x - 40, y: z.y - 40, w: z.w + 80, h: z.h + 80 }, 0.6);
      await L.click([app.pid], z.x + 100, z.y + 100, { button: 'right' });
      const menu = await until(async () => (await list()).find((x) => x.cls === '#32768'), 1500);
      if (L.check('  right click opens the native menu', !!menu)) {
        await shot('pin-menu', { x: menu.x - 20, y: menu.y - 20, w: menu.w + 40, h: menu.h + 40 }, 1);
        await L.key(app.pid, VK.esc);
        await until(async () => !(await list()).find((x) => x.cls === '#32768'), 1000);
      }
      // Between monitors of different scale: the pin is dragged like the main window.
      const other = monitors.find((x) => x !== m && x !== formMonitor);
      if (other) {
        await L.drag([app.pid], z.x + 120, z.y + 120, cx(other), cy(other), { steps: 30, pause: 25 });
        await sleep(500);
        const moved = (await list()).find((x) => x.hwnd === w.hwnd);
        L.check('  dragged to a monitor of another scale, it keeps its logical size', moved && moved.dpi !== z.dpi && Math.abs(moved.w / z.w - moved.dpi / z.dpi) < 0.05, moved ? `${z.w}x${z.h} dpi ${z.dpi} -> ${moved.w}x${moved.h} dpi ${moved.dpi}` : 'gone');
        await shot('pin-on-other-monitor', { x: moved.x - 30, y: moved.y - 30, w: moved.w + 60, h: moved.h + 60 }, 0.8);
        await L.drag([app.pid], moved.x + 80, moved.y + 80, m.x + 800, m.y + 600, { steps: 30, pause: 25 });
        await sleep(500);
        const home = (await list()).find((x) => x.hwnd === w.hwnd);
        z.x = home.x;
        z.y = home.y;
      }
      await L.click([app.pid], z.x + 100, z.y + 100, { button: 'right' });
      await until(async () => (await list()).find((x) => x.cls === '#32768'), 1500);
      await L.key(app.pid, VK.up);
      await L.key(app.pid, VK.enter);
      const closed = await until(async () => !(await list()).find((x) => x.hwnd === w.hwnd), 1500);
      L.check('  the menu closes the pin', closed);
    }
  },

  async main_basic() {
    await P('show-main');
    const w = await until(async () => (await list()).find((x) => x.w > 800 && x.h > 500 && x.w < 3000), 2000);
    if (!L.check('the main window shows', !!w)) return;
    await sleep(500);
    let fg = await call('fg');
    L.check('  and takes the focus', fg.hwnd === w.hwnd, fg.cls);
    const t0 = await call('tick');
    L.check('  no input was injected to get it', t0.lastInput <= L.lastInjected + 500, `${t0.lastInput - L.lastInjected} ms after the last injected`);
    const m = monitors.find((x) => x.primary);
    L.check('  centered on the primary work area', near(w.x, m.wx + (m.ww - w.w) / 2, 2) && near(w.y, m.wy + (m.wh - w.h) / 2, 2), `${w.x},${w.y} of ${m.ww}x${m.wh}, size ${w.w}x${w.h}`);
    const scale = w.dpi / 96;
    const f = w.frame || w;
    L.note(`main ${w.w}x${w.h} dpi ${w.dpi}; thickframe=${w.thickframe}`);
    await shot('main-window', { x: w.x - 30, y: w.y - 30, w: w.w + 60, h: w.h + 60 }, 0.6);
    const hit = async (x, y) => (await call('hit', { hwnd: w.hwnd, x, y })).result;
    const top = f.y + Math.round(16 * scale);
    const row = [];
    for (let dx = 8; dx < 240 * scale; dx += Math.round(8 * scale)) row.push(await hit(f.x + f.w - dx, top));
    L.check('caption hit test: maximize for Snap Layouts, and caption', row.includes(9) && row.includes(2), [...new Set(row)].join(','));
    L.check('resize edges hit test', (await hit(f.x + 1, f.y + Math.floor(f.h / 2))) === 10 && (await hit(f.x + f.w - 2, f.y + Math.floor(f.h / 2))) === 11 && (await hit(f.x + Math.floor(f.w / 2), f.y + f.h - 2)) === 15);
    const S = (dip) => Math.round(dip * scale);
    await L.click([app.pid], f.x + S(72), f.y + S(241));
    await sleep(300);
    let ui = await P('ui-state', { target: 'main' });
    L.check('a real click opens Settings', ui.page === 'settings', ui.page);
    await shot('settings', { x: f.x, y: f.y, w: f.w, h: f.h }, 0.5);
    await L.click([app.pid], f.x + f.w - S(154), f.y + S(171));
    await sleep(200);
    L.check('focusing a hotkey field suspends hotkeys', (await P('state')).hotkeysSuspended === true);
    await L.key(app.pid, VK.esc);
    await sleep(150);
    await L.click([app.pid], f.x + S(600), f.y + S(73));
    await sleep(250);
    L.check('leaving it resumes them', (await P('state')).hotkeysSuspended === false);
    await L.click([app.pid], f.x + f.w - S(93), f.y + S(635));
    const pick = await until(async () => (await list()).find((x) => x.cls === '#32770'), 3000);
    if (L.check('the folder picker opens', !!pick)) {
      await sleep(300);
      await shot('folder-picker', { x: pick.x, y: pick.y, w: pick.w, h: pick.h }, 0.33);
      await L.key(app.pid, VK.esc);
      await until(async () => !(await list()).find((x) => x.cls === '#32770'), 2000);
    }
    await sleep(200);
    await L.wheel([app.pid], f.x + S(600), f.y + S(400), -6);
    await sleep(250);
    await shot('main-scrolled', { x: f.x, y: f.y, w: f.w, h: f.h }, 0.5);
    await L.click([app.pid], f.x + S(72), f.y + S(127));
    await sleep(300);
    // The minimize and close buttons are drawn by the app: real clicks.
    await L.click([app.pid], f.x + f.w - S(115), f.y + S(16));
    await sleep(400);
    const minimized = (await list()).find((x) => x.hwnd === w.hwnd);
    L.check('the minimize button minimizes', minimized && minimized.x < -30000, minimized ? `${minimized.x},${minimized.y}` : 'not listed');
    await P('show-main');
    await sleep(500);
    const back = (await list()).find((x) => x.hwnd === w.hwnd);
    L.check('show restores it to its place', back && back.x === w.x && back.y === w.y, back ? `${back.x},${back.y}` : '');
    fg = await call('fg');
    L.check('  with the focus', fg.hwnd === w.hwnd, fg.cls);
    await L.click([app.pid], f.x + f.w - S(23), f.y + S(16));
    const hidden = await until(async () => !(await P('state')).mainVisible, 1500);
    L.check('Close hides it and the app lives', hidden && (await P('ping')) === 'pong');
    await sleep(200);
    fg = await call('fg');
    L.note('focus after hiding: ' + fg.cls);
    await P('show-main');
    await sleep(400);
    const again = (await list()).find((x) => x.w > 800 && x.h > 500);
    L.check('reopened at the same place', again && again.x === w.x && again.y === w.y, again ? `${again.x},${again.y}` : '');
    await L.key(app.pid, VK.alt, VK.f4);
    await until(async () => !(await P('state')).mainVisible, 1500);
  },

  async main_moves() {
    await P('show-main');
    const w = await until(async () => (await list()).find((x) => x.w > 800 && x.h > 500 && x.w < 3000), 2000);
    if (!L.check('the main window for moving', !!w)) return;
    await sleep(400);
    const find = async () => (await list()).find((x) => x.hwnd === w.hwnd);
    const scale = w.dpi / 96;
    const titleOf = (x) => { const f = x.frame || x; return { x: f.x + Math.floor(f.w / 2), y: f.y + Math.round(16 * (x.dpi / 96)) }; };
    let t = titleOf(w);
    await L.drag([app.pid], t.x, t.y, t.x + 160, t.y + 90);
    await sleep(200);
    let now = await find();
    L.check('dragging by the caption moves it', near(now.x, w.x + 160, 3) && near(now.y, w.y + 90, 3), `${w.x},${w.y} -> ${now.x},${now.y}`);
    const nf = now.frame || now;
    await L.drag([app.pid], nf.x + nf.w - 2, nf.y + Math.floor(nf.h / 2), nf.x + nf.w + 118, nf.y + Math.floor(nf.h / 2));
    await sleep(200);
    const resized = await find();
    L.check('dragging the edge resizes it', near(resized.w, now.w + 120, 6), `${now.w} -> ${resized.w}`);
    now = resized;
    t = titleOf(now);
    const onMon = monitors.find((m) => t.x >= m.x && t.x < m.x + m.w);
    await L.click([app.pid], t.x, t.y, { twice: true });
    await sleep(500);
    const maxed = await find();
    const mf = maxed.frame || maxed;
    L.check('double click on the caption maximizes to the work area', near(mf.w, onMon.ww, 4) && near(mf.h, onMon.wh, 4), `${mf.w}x${mf.h} of ${onMon.ww}x${onMon.wh}`);
    await shot('main-maximized', { x: onMon.x, y: onMon.y, w: onMon.w, h: onMon.h }, 0.3);
    const mt = titleOf(maxed);
    await L.click([app.pid], mt.x, mt.y, { twice: true });
    await sleep(500);
    const restored = await find();
    L.check('and again restores it', near(restored.w, now.w, 4) && near(restored.h, now.h, 4), `${restored.w}x${restored.h}`);
    // The maximize button itself, the one Snap Layouts hangs on.
    const rf = restored.frame || restored;
    const maxX = rf.x + rf.w - Math.round(69 * scale), btnY = rf.y + Math.round(16 * scale);
    L.check('the maximize button is where it should be', (await call('hit', { hwnd: w.hwnd, x: maxX, y: btnY })).result === 9);
    await L.move(maxX, btnY);
    await sleep(1100);
    await shot('snap-layouts', { x: rf.x + rf.w - Math.round(420 * scale), y: rf.y - 10, w: Math.round(460 * scale), h: Math.round(260 * scale) }, 0.8);
    await L.click([app.pid], maxX, btnY);
    await sleep(500);
    const byButton = await find();
    L.check('a click on it maximizes', near((byButton.frame || byButton).w, onMon.ww, 4), `${(byButton.frame || byButton).w}`);
    const bf = byButton.frame || byButton;
    await L.click([app.pid], bf.x + bf.w - Math.round(69 * scale), bf.y + Math.round(16 * scale));
    await sleep(500);
    const again = await find();
    L.check('and again restores it', near(again.w, restored.w, 4), `${again.w}x${again.h}`);
    // Between monitors of different scale.
    const other = monitors.find((m) => m !== onMon && m !== formMonitor);
    if (other) {
      const from = titleOf(again);
      await L.assertTarget([app.pid], from.x, from.y);
      await L.move(from.x, from.y);
      await sleep(80);
      await L.guard();
      L.mark(await call('btn', { flags: 0x0002 }));
      L.buttonDown = true;
      const to = { x: cx(other), y: other.y + 150 };
      let last = '';
      try {
        for (let i = 1; i <= 40; i++) {
          await L.guard();
          L.mark(await call('move', { x: Math.round(from.x + ((to.x - from.x) * i) / 40), y: Math.round(from.y + ((to.y - from.y) * i) / 40) }));
          await sleep(35);
          const x = await find();
          const k = `${x.w}x${x.h} dpi ${x.dpi}`;
          if (k !== last) { L.note(`  step ${i}: ${k}`); last = k; }
        }
        await sleep(250);
      } finally {
        L.mark(await call('btn', { flags: 0x0004 }));
        L.buttonDown = false;
      }
      await sleep(500);
      const moved = await find();
      L.check('dragged to a monitor of another scale, the DPI follows', moved.dpi !== w.dpi, `${w.dpi} -> ${moved.dpi}, ${again.w}x${again.h} -> ${moved.w}x${moved.h}`);
      const ratio = moved.w / again.w, expected = moved.dpi / again.dpi;
      L.check('  and the logical size stays', Math.abs(ratio - expected) < 0.05, `ratio ${ratio.toFixed(2)} expected ${expected.toFixed(2)}`);
      await shot('main-on-other-monitor', { x: moved.x - 20, y: moved.y - 20, w: moved.w + 40, h: moved.h + 40 }, 0.6);
      const back = titleOf(moved);
      await L.drag([app.pid], back.x, back.y, cx(onMon), onMon.y + 200, { steps: 30, pause: 25 });
      await sleep(500);
      const home = await find();
      L.check('dragged back, the size returns', home.dpi === again.dpi && near(home.w, again.w, 8) && near(home.h, again.h, 8), `${home.w}x${home.h} dpi ${home.dpi}`);
    }
    await L.key(app.pid, VK.alt, VK.f4);
    await until(async () => !(await P('state')).mainVisible, 1500);
  },

  async tray() {
    const t = await call('tray', { cls: 'Shotik.NativeTray', pid: app.pid });
    if (!L.check('tray window found', t.found, JSON.stringify(t))) return;
    const WM_APP2 = 0x8002, WM_CONTEXTMENU = 0x7B, NIN_SELECT = 0x400;
    const xy = (t.x + 4) | ((t.y + 4) << 16);
    await call('post', { hwnd: t.hwnd, msg: WM_APP2, w: xy, l: WM_CONTEXTMENU });
    const menu = await until(async () => (await list()).find((x) => x.cls === '#32768'), 2000);
    if (L.check('the tray icon context menu opens', !!menu)) {
      await sleep(200);
      await shot('tray-menu', { x: menu.x - 20, y: menu.y - 20, w: menu.w + 40, h: menu.h + 40 }, 1);
      const fg = await call('fg');
      L.check('  the menu has the foreground, so a click away closes it', fg.pid === app.pid, fg.cls);
      await L.key(app.pid, VK.esc);
      await until(async () => !(await list()).find((x) => x.cls === '#32768'), 1000);
    }
    await call('post', { hwnd: t.hwnd, msg: WM_APP2, w: xy, l: NIN_SELECT });
    const shown = await until(async () => (await P('state')).mainVisible, 2000);
    L.check('a left click on the icon opens the main window', shown);
    if (shown) {
      await sleep(300);
      const fg = await call('fg');
      L.check('  with the focus', fg.pid === app.pid, fg.cls);
      await L.key(app.pid, VK.alt, VK.f4);
      const hidden = await until(async () => !(await P('state')).mainVisible, 1500);
      L.check('  Alt+F4 hides it like Close', hidden && (await P('ping')) === 'pong');
    }
    const created = await call('regmsg', { name: 'TaskbarCreated' });
    await call('post', { hwnd: t.hwnd, msg: created.id, w: 0, l: 0 });
    await sleep(600);
    const after = await call('tray', { cls: 'Shotik.NativeTray', pid: app.pid });
    L.check('after Explorer would restart, the icon is back', after.found && after.hr === 0 && after.w > 0, JSON.stringify(after));
  },

  async toast_hover() {
    const r = await P('show-toast', { kind: 'text', title: 'Live', body: 'hover test' });
    const toast = await until(async () => (await list()).find((w) => w.w < 900 && w.h < 300), 800);
    if (!L.check('a toast appears', r.visible && toast)) return;
    await L.move(toast.x + 40, toast.y + 30);
    await sleep(6000);
    L.check('it stays while the pointer is on it', (await P('toast-visible')).visible);
    await L.move(toast.x - 300, toast.y - 300);
    const gone = await until(async () => !(await P('toast-visible')).visible, 9000);
    L.check('it leaves after the pointer does', gone);
  },

  async translate_https() {
    await P('show-main');
    await sleep(400);
    await P('ui-action', { target: 'main', action: { type: 'quick-translate', text: 'Good morning, world' } });
    const done = await until(async () => {
      const s = await P('ui-state', { target: 'main' });
      return s.text && !s.text.loading && s.text.resultVisible ? s : null;
    }, 15000, 200);
    L.check('a real translation over HTTPS', done && /[а-яё]/i.test(done.text.result), done ? done.text.result.slice(0, 80) : 'no result');
    const settings = await P('settings-set', { patch: {} });
    L.note('provider ' + JSON.stringify(settings.translate && settings.translate.provider));
    await L.key(app.pid, VK.alt, VK.f4).catch(() => {});
    await until(async () => !(await P('state')).mainVisible, 1500);
  },

  async selection() {
    const w = asList((await call('windows', { pid: mine.pid })).windows)[0];
    if (!L.check('the check window for selection', !!w)) return;
    const savedClip = await call('clip');
    const x = w.x + 150, y = w.y + 70;
    await L.click([mine.pid], x, y, { twice: true });
    const bubble = await until(async () => (await list()).find((b) => b.w <= 140 && b.h <= 140 && b.topmost), 4000, 60);
    if (!L.check('a double-click selection brings the bubble (UI Automation)', !!bubble, bubble ? `${bubble.w}x${bubble.h} at ${bubble.x},${bubble.y}` : '')) {
      const log = path.join(app.temp, 'shotik-bubble.log');
      L.note('diag: ' + (fs.existsSync(log) ? fs.readFileSync(log, 'utf8').split(String.fromCharCode(10)).slice(-14).join(' | ') : 'no log'));
      L.note('bubble ' + JSON.stringify(await P('ui-state', { target: 'bubble' }).catch((e) => e.message)));
      L.note('windows ' + JSON.stringify((await list()).map((x) => [x.cls, x.w, x.h, x.x, x.y])));
      L.note('fg ' + (await call('fg')).cls + ' mine pid ' + mine.pid + ' fg pid ' + (await call('fg')).pid);
      return;
    }
    await shot('bubble', { x: bubble.x - 60, y: bubble.y - 60, w: bubble.w + 120, h: bubble.h + 120 }, 1);
    const fgBefore = await call('fg');
    L.check('  focus stays on the app with the selection', fgBefore.pid === mine.pid, fgBefore.cls);
    await L.click([app.pid], bubble.x + Math.floor(bubble.w / 2), bubble.y + Math.floor(bubble.h / 2));
    const popup = await until(async () => {
      const s = await P('ui-state', { target: 'tp' }).catch(() => null);
      return s && s.translated && !s.loading ? s : null;
    }, 12000, 150);
    L.check('its click translates the selected word', popup && popup.original && popup.translated, popup ? `${popup.original} -> ${popup.translated}` : 'no popup');
    await shot('translate-popup', null, 0.33);
    const clipAfter = await call('clip');
    L.check('  the clipboard was not touched', clipAfter.formats === savedClip.formats && clipAfter.text === savedClip.text, clipAfter.formats);
    const fgAfter = await call('fg');
    L.note('foreground with the popup: ' + (fgAfter.pid === app.pid ? 'Shotik' : fgAfter.pid === mine.pid ? 'test window' : fgAfter.cls));
    await L.click([mine.pid], w.x + 400, w.y + 300);
    const hid = await until(async () => !(await P('tp-visible').catch(() => ({}))).visible, 3000);
    L.note('popup after a click elsewhere: ' + (hid ? 'hidden' : 'still visible (it has a close button, by the contract)'));
    await L.click([mine.pid], x, y, { twice: true });
    await sleep(500);
    await L.chord(...hk(VK.f5));
    const byKey = await until(async () => {
      const s = await P('ui-state', { target: 'tp' }).catch(() => null);
      return s && s.translated && !s.loading ? s : null;
    }, 12000, 150);
    L.check('the translate hotkey reads the selection', byKey && byKey.original, byKey ? byKey.original : '');
    const clipEnd = await call('clip');
    L.check('  and restores the clipboard', clipEnd.formats === savedClip.formats && clipEnd.text === savedClip.text, clipEnd.formats);
    await P('bubble-clear').catch(() => {});
  },

  async ocr_live() {
    const w = asList((await call('windows', { pid: mine.pid })).windows)[0];
    const m = monitors.find((x) => w.x >= x.x && w.x < x.x + x.w);
    const { ok } = await openOverlay(m, VK.f8);
    if (!L.check('the text hotkey opens the overlay', ok)) return;
    await L.drag([app.pid], w.x + 10, w.y + 40, w.x + w.w - 20, w.y + w.h - 40);
    const s = await until(async () => { const o = await overlayState(); return o.textMode && o.wordCount > 0 ? o : null; }, 4000, 100);
    L.check('Live Text reads the real screen', !!s, s ? `${s.wordCount} words` : '');
    await shot('live-text', { x: w.x - 20, y: w.y - 20, w: w.w + 40, h: w.h + 40 }, 0.6);
    await L.key(app.pid, VK.esc);
    await sleep(150);
    await closeOverlay();
  },

  async record_video() {
    await P('settings-set', { patch: { recording: { systemAudio: true, microphone: false, countdown: true } } });
    const m = monitors.find((x) => x.primary);
    const { ok } = await openOverlay(m, VK.f7);
    if (!L.check('the record hotkey opens the overlay', ok)) return;
    await L.drag([app.pid], m.x + 500, m.y + 400, m.x + 1300, m.y + 1000);
    // In record mode the selection itself starts the recording.
    await sleep(500);
    await shot('countdown', null, 0.33);
    const rec = await until(async () => (await P('record-status')).recording, 7000, 100);
    if (!L.check('recording starts after the countdown', rec)) return;
    await sleep(400);
    const wins = await list();
    for (const w of wins) L.note(`  ${w.w}x${w.h} at ${w.x},${w.y} topmost=${w.topmost} clickthrough=${w.transparent} affinity=${w.affinity} noactivate=${w.noactivate}`);
    const fg = await call('fg');
    L.check('recording windows take no focus', fg.pid !== app.pid, fg.cls);
    await shot('recording', null, 0.33);
    const controls = wins.find((w) => w.h < 120 && w.w > 200);
    L.check('the controls bar is out of the recording', controls && controls.affinity === 17, controls ? String(controls.affinity) : 'none');
    const borders = wins.filter((w) => w !== controls && w.transparent);
    L.check('the border is click-through and out of the recording', borders.length >= 1 && borders.every((b) => b.affinity === 17), borders.map((b) => b.affinity).join(','));
    const mid = await P('ui-state', { target: 'rec-controls' });
    L.note('meters ' + JSON.stringify(mid.meters).slice(0, 160));
    await sleep(3000);
    await L.chord(...hk(VK.f7));
    const stopped = await until(async () => !(await P('record-status')).recording, 5000, 100);
    L.check('the hotkey stops it', stopped);
    await sleep(500);
    const st = await P('record-status');
    const item = asList(await P('history-list', { limit: 1 }))[0];
    const file = item && (item.path || item.file);
    if (!L.check('the video is saved', file && fs.existsSync(file), file ? path.basename(file) : JSON.stringify(st).slice(0, 200))) return;
    const probe = JSON.parse(execFileSync('ffprobe', ['-v', 'error', '-show_entries', 'stream=codec_name,codec_type,width,height,sample_rate,channels:format=duration', '-of', 'json', file], { encoding: 'utf8' }));
    const video = probe.streams.find((s) => s.codec_type === 'video');
    const audio = probe.streams.find((s) => s.codec_type === 'audio');
    L.check('H.264 at the selected size', video && video.codec_name === 'h264' && near(video.width, 800, 2) && near(video.height, 600, 2), JSON.stringify(video));
    L.check('AAC 48 kHz from a device at another rate', audio && audio.codec_name === 'aac' && audio.sample_rate === '48000', JSON.stringify(audio));
    L.check('duration matches', near(Number(probe.format.duration), 3.5, 1.2), probe.format.duration);
    const frame = path.join(shots, 'video-frame.png');
    execFileSync('ffmpeg', ['-v', 'error', '-y', '-ss', '1', '-i', file, '-frames:v', '1', frame]);
    L.note('video frame written for review');
    const trace = st.trace || [];
    L.note('trace: ' + trace.filter((l) => /Hz|rate|loopback|device|codec|vtracks/i.test(l)).slice(0, 8).join(' | ').slice(0, 500));
  },

  async record_audio() {
    const rate = 48000, seconds = 5, tone = 1000, amp = Math.pow(10, -46 / 20);
    const samples = rate * seconds;
    const buf = Buffer.alloc(44 + samples * 2);
    buf.write('RIFF', 0); buf.writeUInt32LE(36 + samples * 2, 4); buf.write('WAVEfmt ', 8); buf.writeUInt32LE(16, 16);
    buf.writeUInt16LE(1, 20); buf.writeUInt16LE(1, 22); buf.writeUInt32LE(rate, 24); buf.writeUInt32LE(rate * 2, 28);
    buf.writeUInt16LE(2, 32); buf.writeUInt16LE(16, 34); buf.write('data', 36); buf.writeUInt32LE(samples * 2, 40);
    for (let i = 0; i < samples; i++) buf.writeInt16LE(Math.round(amp * 32767 * Math.sin((2 * Math.PI * tone * i) / rate)), 44 + i * 2);
    const wav = path.join(shots, 'tone.wav');
    fs.writeFileSync(wav, buf);
    await P('settings-set', { patch: { audioRecording: { systemAudio: true, microphone: false } } });
    await L.chord(...hk(VK.f6));
    const rec = await until(async () => (await P('record-status')).recording, 4000, 80);
    if (!L.check('the audio hotkey starts recording', rec)) return;
    const player = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', `(New-Object Media.SoundPlayer '${wav}').PlaySync()`], { stdio: 'ignore', windowsHide: true });
    await sleep(800);
    const ui = await P('ui-state', { target: 'rec-controls' });
    L.note('meters ' + JSON.stringify(ui.meters).slice(0, 160));
    await sleep(3500);
    await L.chord(...hk(VK.f6));
    const stopped = await until(async () => !(await P('record-status')).recording, 4000, 80);
    L.check('the hotkey stops it', stopped);
    await new Promise((r) => { if (player.exitCode !== null) r(); else player.once('exit', r); });
    await sleep(400);
    const item = asList(await P('history-list', { limit: 1 }))[0];
    const file = item && (item.path || item.file);
    if (!L.check('an .m4a is saved', file && /\.m4a$/i.test(file) && fs.existsSync(file), file ? path.basename(file) : '')) return;
    const probe = JSON.parse(execFileSync('ffprobe', ['-v', 'error', '-show_entries', 'stream=codec_name,sample_rate,channels:format=duration', '-of', 'json', file], { encoding: 'utf8' }));
    L.note('audio ' + JSON.stringify(probe.streams[0]) + ' duration ' + probe.format.duration);
    const pcm = execFileSync('ffmpeg', ['-v', 'error', '-i', file, '-ac', '1', '-ar', '48000', '-f', 's16le', '-'], { maxBuffer: 1 << 27 });
    const s = new Int16Array(pcm.buffer, pcm.byteOffset, Math.floor(pcm.length / 2));
    const from = 48000, to = Math.min(s.length, 48000 * 3);
    const power = (f) => { let re = 0, im = 0; for (let i = from; i < to; i++) { re += s[i] * Math.cos(2 * Math.PI * f * i / 48000); im += s[i] * Math.sin(2 * Math.PI * f * i / 48000); } return Math.hypot(re, im) / ((to - from) / 2); };
    let best = 0, bestF = 0;
    for (let f = 300; f <= 3000; f += 5) { const p = power(f); if (p > best) { best = p; bestF = f; } }
    const db = 20 * Math.log10(best / 32767);
    L.check('the played 1 kHz tone comes back at 1 kHz, so the 192 kHz path resamples right', Math.abs(bestF - 1000) <= 5, `${bestF} Hz at ${db.toFixed(1)} dBFS, played at -46 dBFS`);
    const trace = (await P('record-status')).trace || [];
    L.note('trace: ' + trace.filter((l) => /Hz|rate|loopback|device/i.test(l)).slice(0, 6).join(' | ').slice(0, 400));
  },

  async microphone() {
    await P('settings-set', { patch: { audioRecording: { systemAudio: false, microphone: true } } });
    await L.chord(...hk(VK.f6));
    const rec = await until(async () => (await P('record-status')).recording, 4000, 80);
    if (!L.check('recording from the real microphone starts', rec)) return;
    await sleep(2000);
    const ui = await P('ui-state', { target: 'rec-controls' });
    L.note('meters ' + JSON.stringify(ui.meters).slice(0, 160));
    await L.chord(...hk(VK.f6));
    await until(async () => !(await P('record-status')).recording, 4000, 80);
    await sleep(300);
    const st = await P('record-status');
    L.check('no microphone error', !st.lastError, String(st.lastError || ''));
    const item = asList(await P('history-list', { limit: 1 }))[0];
    const file = item && (item.path || item.file);
    if (L.check('a file with audio is saved', file && fs.existsSync(file) && fs.statSync(file).size > 1000, file ? `${fs.statSync(file).size} bytes` : '')) {
      const pcm = execFileSync('ffmpeg', ['-v', 'error', '-i', file, '-ac', '1', '-ar', '48000', '-f', 's16le', '-'], { maxBuffer: 1 << 27 });
      const s = new Int16Array(pcm.buffer, pcm.byteOffset, Math.floor(pcm.length / 2));
      let sum = 0; for (const v of s) sum += v * v;
      L.note(`microphone level ${(20 * Math.log10(Math.sqrt(sum / s.length) / 32767 + 1e-9)).toFixed(1)} dBFS over ${(s.length / 48000).toFixed(1)} s; the file is deleted with the profile`);
      fs.rmSync(file, { force: true });
    }
    await P('settings-set', { patch: { audioRecording: { systemAudio: true, microphone: false } } });
  },

  async microphone_signal() {
    // The virtual cable is a recording device nobody hears: a real signal through the exact-device path.
    const id = execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
      "Get-ChildItem 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\MMDevices\\Audio\\Capture' | ForEach-Object { $g = $_.PSChildName; $p = Get-ItemProperty (Join-Path $_.PSPath 'Properties'); $n = $p.'{a45c254e-df1c-4efd-8020-67d146a850e0},2'; $state = (Get-ItemProperty $_.PSPath).DeviceState; if ($n -like '*CABLE Output*' -and $state -eq 1) { '{0.0.1.00000000}.' + $g } } | Select-Object -First 1"], { encoding: 'utf8', windowsHide: true }).trim();
    if (!L.check('the virtual cable is there', /^\{0\.0\.1/.test(id), id)) return;
    await P('settings-set', { patch: { audioRecording: { systemAudio: false, microphone: true }, recording: { microphoneDeviceId: id } } });
    const tone = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', path.join(SCRATCH, 'cable-tone.ps1'), '-Seconds', '5', '-Freq', '880', '-AmplitudeDb', '-20'], { stdio: ['ignore', 'pipe', 'ignore'], windowsHide: true });
    let played = '';
    tone.stdout.on('data', (d) => (played += d));
    await sleep(700);
    await L.chord(...hk(VK.f6));
    const rec = await until(async () => (await P('record-status')).recording, 4000, 80);
    if (!L.check('recording from the selected device starts', rec)) return;
    await sleep(1500);
    const ui = await P('ui-state', { target: 'rec-controls' });
    const meter = asList(ui.meters).find((x) => x.input === 'microphone');
    L.check('its level meter moves', meter && meter.fill > 0, JSON.stringify(ui.meters).slice(0, 120));
    await sleep(1500);
    await L.chord(...hk(VK.f6));
    await until(async () => !(await P('record-status')).recording, 4000, 80);
    await new Promise((r) => { if (tone.exitCode !== null) r(); else tone.once('exit', r); });
    L.note('tone: ' + played.trim());
    await sleep(300);
    const item = asList(await P('history-list', { limit: 1 }))[0];
    const file = item && (item.path || item.file);
    if (!L.check('a file is saved', file && fs.existsSync(file), file ? path.basename(file) : '')) return;
    const pcm = execFileSync('ffmpeg', ['-v', 'error', '-i', file, '-ac', '1', '-ar', '48000', '-f', 's16le', '-'], { maxBuffer: 1 << 27 });
    const x = new Int16Array(pcm.buffer, pcm.byteOffset, Math.floor(pcm.length / 2));
    const from = 24000, to = Math.min(x.length, 48000 * 2);
    const power = (f) => { let re = 0, im = 0; for (let i = from; i < to; i++) { re += x[i] * Math.cos(2 * Math.PI * f * i / 48000); im += x[i] * Math.sin(2 * Math.PI * f * i / 48000); } return Math.hypot(re, im) / ((to - from) / 2); };
    let best = 0, bestF = 0;
    for (let f = 300; f <= 3000; f += 5) { const pw = power(f); if (pw > best) { best = pw; bestF = f; } }
    L.check('the tone comes through the microphone path at its pitch', Math.abs(bestF - 880) <= 5, `${bestF} Hz at ${(20 * Math.log10(best / 32767 + 1e-9)).toFixed(1)} dBFS, played at -20 dBFS`);
    await P('settings-set', { patch: { audioRecording: { systemAudio: true, microphone: false }, recording: { microphoneDeviceId: 'default' } } });
  },

  async focus_without_input() {
    // The way Claude opens the overlay: no key press reached this process, so Windows locks the foreground.
    const form = asList((await call('windows', { pid: mine.pid })).windows)[0];
    await L.click([mine.pid], form.x + form.w - 60, form.y + form.h - 40);
    await sleep(300);
    const m = formMonitor;
    await L.move(cx(m), m.y + 40);
    await sleep(100);
    P('trigger', { mode: 'region' }).catch(() => {});
    const opened = await until(async () => {
      const st = await P('state');
      if (st.overlayCount !== monitors.length) return false;
      return (await call('fg')).pid === app.pid;
    }, 3000, 30);
    L.check('an overlay opened with no key press gets the keyboard', opened, (await call('fg')).cls);
    await closeOverlay();
    await sleep(200);
    await L.click([mine.pid], form.x + form.w - 60, form.y + form.h - 40);
    await sleep(300);
    await P('show-main');
    const shown = await until(async () => { const f = await call('fg'); return f.pid === app.pid && f.w > 800; }, 2500, 40);
    L.check('so does the main window', shown, (await call('fg')).cls);
    const fgNow = await call('fg');
    if (fgNow.pid === app.pid) await L.key(app.pid, VK.alt, VK.f4);
    await until(async () => !(await P('state')).mainVisible, 1500);
    await sleep(200);
  },

  async overlay_keys() {
    const m = monitors.find((x) => !x.primary) || monitors[0];
    // F takes the whole screen.
    let { ok } = await openOverlay(m);
    if (!L.check('overlay for its keys', ok)) return;
    await L.key(app.pid, VK.f);
    let s = await overlayState();
    L.check('F selects the whole monitor', s.selection && near(s.selection.w, m.w, 1) && near(s.selection.h, m.h, 1), JSON.stringify(s.selection));
    await L.key(app.pid, VK.esc);
    await sleep(120);
    await closeOverlay();
    // C copies the colour under the cursor.
    ({ ok } = await openOverlay(m));
    if (ok) {
      await L.move(m.x + 300, m.y + 300);
      await sleep(120);
      await L.key(app.pid, VK.c);
      await until(async () => !(await P('state')).overlayOpen, 1500);
      await sleep(200);
      const clip = await call('clip');
      L.check('C copies the colour under the cursor', /^#[0-9a-f]{6}$/i.test(clip.text || ''), clip.text || 'no text');
    }
    // A copies for Claude: picture and path.
    ({ ok } = await openOverlay(m));
    if (ok) {
      await L.drag([app.pid], m.x + 200, m.y + 200, m.x + 600, m.y + 500);
      await L.key(app.pid, VK.a);
      await until(async () => !(await P('state')).overlayOpen, 1500);
      await sleep(300);
      const clip = await call('clip');
      L.check('A puts both the picture and its path on the clipboard', clip.hasImage && /\.png/i.test(clip.text || ''), `image ${clip.hasImage}, text ${clip.text ? path.extname(clip.text) : 'none'}`);
    }
    // A double click inside the selection copies.
    ({ ok } = await openOverlay(m));
    if (ok) {
      await L.drag([app.pid], m.x + 250, m.y + 250, m.x + 650, m.y + 450);
      await L.click([app.pid], m.x + 400, m.y + 350, { twice: true });
      const closed = await until(async () => !(await P('state')).overlayOpen, 1500);
      await sleep(250);
      const clip = await call('clip');
      L.check('a double click inside the selection copies it', closed && clip.hasImage && near(clip.w, 400, 3) && near(clip.h, 200, 3), `${clip.w}x${clip.h}`);
    }
  },

  async toolbar_mouse() {
    const m = monitors.find((x) => !x.primary) || monitors[0];
    const { ok } = await openOverlay(m);
    if (!L.check('overlay for the toolbar', ok)) return;
    await L.drag([app.pid], m.x + 400, m.y + 200, m.x + 1000, m.y + 600);
    // At 100% the toolbar is centred under the selection, 720 wide, with its middle 36 below it.
    const centre = m.x + 700, row = m.y + 636;
    await L.click([app.pid], centre - 305, row);
    await sleep(150);
    let s = await overlayState();
    L.check('a real click on the pen button selects the pen', s.tool === 'pen', s.tool);
    await L.drag([app.pid], m.x + 500, m.y + 300, m.x + 800, m.y + 500, { steps: 12 });
    s = await overlayState();
    L.check('and it draws', s.annotationCount === 1, String(s.annotationCount));
    await shot('toolbar', { x: m.x + 300, y: m.y + 150, w: 900, h: 560 }, 1);
    await L.click([app.pid], centre + 300, row);
    const closed = await until(async () => !(await P('state')).overlayOpen, 2000);
    await sleep(300);
    const clip = await call('clip');
    L.check('a real click on the copy button copies with the annotation', closed && clip.hasImage && near(clip.w, 600, 3) && near(clip.h, 400, 3), `${clip.w}x${clip.h}`);
  },

  async russian_layout() {
    const m = monitors.find((x) => x.primary);
    const { ok, } = await openOverlay(m);
    if (!L.check('overlay for the Russian layout', ok)) return;
    const fg = await call('fg');
    const original = fg.layout;
    const WM_INPUTLANGCHANGEREQUEST = 0x50, RUSSIAN = 0x04190419;
    await call('post', { hwnd: fg.hwnd, msg: WM_INPUTLANGCHANGEREQUEST, w: 0, l: RUSSIAN });
    await sleep(300);
    const now = await call('fg');
    try {
      if (!L.check('the overlay thread switched to Russian', now.layout === '04190419', now.layout)) return;
      await L.drag([app.pid], m.x + 300, m.y + 300, m.x + 1500, m.y + 1100);
      await L.key(app.pid, digit(2));
      let s = await overlayState();
      L.check('in Russian, the digit keys still pick tools', s.tool === 'pen', s.tool);
      await L.drag([app.pid], m.x + 500, m.y + 500, m.x + 900, m.y + 800, { steps: 12 });
      await L.key(app.pid, VK.ctrl, VK.z);
      s = await overlayState();
      L.check('Ctrl+Z works by physical key', s.annotationCount === 0, String(s.annotationCount));
      await L.key(app.pid, digit(9));
      await L.click([app.pid], m.x + 1000, m.y + 500);
      await sleep(120);
      for (const key of [0x47, 0x48, 0x42, 0x44, 0x54, 0x4E]) await L.key(app.pid, key); // g h b d t n
      await sleep(150);
      s = await overlayState();
      L.check('physical keys type Russian letters in the editor', s.editorText === 'привет', s.editorText);
      await L.key(app.pid, VK.esc);
      await sleep(100);
      await L.key(app.pid, digit(1));
      const before = (await overlayState()).selection;
      await L.key(app.pid, VK.p);
      const closed = await until(async () => !(await P('state')).overlayOpen, 1500);
      L.check('P pins by physical key', closed && before, 'closed ' + closed);
      await sleep(300);
      const pin = (await list()).find((x) => x.w > 300);
      if (pin) {
        await L.click([app.pid], pin.x + 100, pin.y + 100, { button: 'right' });
        await until(async () => (await list()).find((x) => x.cls === '#32768'), 1500);
        await L.key(app.pid, VK.up);
        await L.key(app.pid, VK.enter);
        await until(async () => !(await list()).find((x) => x.hwnd === pin.hwnd), 1500);
      }
    } finally {
      await closeOverlay();
      await sleep(150);
      // The layout is global on this Windows: ask whatever window has the keyboard now to change it back.
      for (let i = 0; i < 5 && (await call('fg')).layout !== original; i++) {
        await call('post', { hwnd: (await call('fg')).hwnd, msg: WM_INPUTLANGCHANGEREQUEST, w: 0, l: parseInt(original.slice(0, 8), 16) });
        await sleep(300);
      }
      const restored = (await call('fg')).layout;
      L.check('the keyboard layout is back as it was', restored === original, `${restored}, it was ${original}`);
    }
  },

  async record_controls() {
    await P('settings-set', { patch: { recording: { systemAudio: false, microphone: false, countdown: false } } });
    const m = monitors.find((x) => x.primary);
    const { ok } = await openOverlay(m, VK.f7);
    if (!L.check('overlay to record', ok)) return;
    await L.drag([app.pid], m.x + 500, m.y + 400, m.x + 1300, m.y + 1000);
    const rec = await until(async () => (await P('record-status')).recording, 4000, 80);
    if (!L.check('recording starts without a countdown', rec)) return;
    await sleep(500);
    const controls = (await list()).find((w) => w.h < 120 && w.w > 200 && !w.transparent);
    if (!L.check('the controls are there', !!controls)) return;
    // Measured from the right edge in DIP: see rec_controls.slint.
    const scale = controls.dpi / 96;
    const at = (fromRight) => ({ x: controls.x + controls.w - Math.round(fromRight * scale), y: controls.y + Math.round(24 * scale) });
    const pause = at(96), stop = at(60);
    await L.click([app.pid], pause.x, pause.y);
    const paused = await until(async () => (await P('record-status')).paused === true, 1500, 60);
    L.check('a real click on Pause pauses', paused);
    await sleep(600);
    await shot('controls-paused', { x: controls.x - 20, y: controls.y - 20, w: controls.w + 40, h: controls.h + 40 }, 1);
    await L.click([app.pid], pause.x, pause.y);
    const resumed = await until(async () => (await P('record-status')).paused === false, 1500, 60);
    L.check('and again resumes', resumed);
    await sleep(800);
    await L.click([app.pid], stop.x, stop.y);
    const stopped = await until(async () => !(await P('record-status')).recording, 4000, 80);
    L.check('a real click on Stop stops', stopped);
    await sleep(400);
    const item = asList(await P('history-list', { limit: 1 }))[0];
    const file = item && (item.path || item.file);
    L.check('the file is saved', file && fs.existsSync(file), file ? path.basename(file) : '');
    L.check('the controls are gone', !(await list()).find((w) => w.hwnd === controls.hwnd));
    await P('settings-set', { patch: { recording: { systemAudio: true, microphone: false, countdown: true } } });
  },

  async second_instance() {
    const r = spawnSync(EXE, ['--capture', 'region'], { env: app.env, timeout: 10000, windowsHide: true });
    L.check('a second start forwards and exits', r.status === 0, `status ${r.status}`);
    const opened = await until(async () => (await P('state')).overlayOpen, 3000);
    L.check('  and the first instance opens the overlay', opened);
    await closeOverlay();
    const r2 = spawnSync(EXE, [], { env: app.env, timeout: 10000, windowsHide: true });
    const shown = await until(async () => (await P('state')).mainVisible, 2000);
    L.check('a plain second start shows the main window', r2.status === 0 && shown);
    if (shown) { await sleep(200); await L.key(app.pid, VK.alt, VK.f4).catch(() => {}); await until(async () => !(await P('state')).mainVisible, 1500); }
  },

  async snap_again() { await sections.snap(); },

  async stability() {
    const m = monitors.find((x) => x.primary);
    const first = await call('proc', { pid: app.pid });
    const shows = [];
    for (let i = 0; i < 25; i++) {
      const { ok } = await openOverlay(m);
      if (!ok) { L.check(`overlay cycle ${i}`, false); return; }
      const marks = await P('overlay-timing');
      shows.push(Number(marks.find((x) => x.startsWith('show=')).split('=')[1]));
      await L.key(app.pid, VK.esc);
      await until(async () => !(await P('state')).overlayOpen, 1500);
      await sleep(120);
    }
    await sleep(3000);
    const last = await call('proc', { pid: app.pid });
    const sorted = [...shows].sort((a, b) => a - b);
    L.note(`real show(): median ${sorted[12]} ms, min ${sorted[0]}, max ${sorted[24]}`);
    L.note(`handles ${first.handles} -> ${last.handles}, GDI ${first.gdi} -> ${last.gdi}, USER ${first.user} -> ${last.user}, private ${first.privateMB} -> ${last.privateMB} MB, threads ${first.threads} -> ${last.threads}`);
    L.check('25 real sessions leave no handle growth', last.handles - first.handles <= 25 && last.gdi - first.gdi <= 20 && last.user - first.user <= 20);
  },
};

(async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-live-'));
  shots = path.join(dir, 'shots');
  fs.mkdirSync(shots, { recursive: true });
  console.log('artifacts ' + shots);
  let clip;
  let failed = 0;
  try {
    const begun = await L.begin();
    console.log(`idle ${Math.round(begun.idle)} s, foreground class ${begun.priorApp}`);
    monitors = asList((await call('monitors')).monitors);
    clip = await preserveClipboard();
    app = await launchApp({ exe: EXE, port: PORT, hotkeys: HOTKEYS, dir });
    formMonitor = monitors.find((m) => !m.primary) || monitors[0];
    mine = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', path.join(SCRATCH, 'testwin.ps1'),
      '-X', String(formMonitor.x + 200), '-Y', String(formMonitor.y + 200), '-W', '1300', '-H', '620'], { stdio: 'ignore', windowsHide: true });
    await until(async () => asList((await call('windows', { pid: mine.pid })).windows).length > 0, 8000, 100);
    await sleep(400);
    const form = asList((await call('windows', { pid: mine.pid })).windows)[0];
    if (!form) throw new Error('the check window did not appear');
    await L.click([mine.pid], form.x + form.w - 60, form.y + form.h - 40);
    await sleep(200);
    L.prior = await call('fg');
    if (L.prior.pid !== mine.pid) throw new Error('the check window did not take the focus: ' + L.prior.cls);
    const order = Object.keys(sections);
    for (const name of order) {
      if (ONLY && !ONLY.includes(name) && name !== 'start') continue;
      console.log('\n== ' + name);
      try { await sections[name](); }
      catch (e) {
        if (e instanceof UserBack) throw e;
        L.check(`${name}: no exception`, false, e.message.slice(0, 200));
        await closeOverlay();
      }
      await L.guard();
    }
    failed = L.summary();
  } catch (e) {
    if (e instanceof UserBack) {
      console.log('\nUSER RETURNED, STOPPED: ' + e.message);
      L.summary();
      process.exitCode = 3;
    } else {
      console.log('\nFAILED: ' + e.stack);
      process.exitCode = 1;
    }
  } finally {
    try { if (L.buttonDown) await call('btn', { flags: 0x0004 }); } catch (_) {}
    if (app) { await stopApp(PORT, app); }
    if (mine) { try { execFileSync('taskkill', ['/F', '/T', '/PID', String(mine.pid)], { stdio: 'ignore', windowsHide: true }); } catch (_) {} }
    if (clip) { try { await clip.restore(); } catch (e) { console.log('CLIPBOARD NOT RESTORED: ' + e.message); } }
    L.probe.close();
    for (const name of ['appdata', 'temp', 'saved']) fs.rmSync(path.join(dir, name), { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    console.log('kept for review, delete after: ' + shots);
  }
  if (failed && !process.exitCode) process.exitCode = 2;
})();
