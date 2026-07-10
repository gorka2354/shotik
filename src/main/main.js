'use strict';
const {
  app, BrowserWindow, Tray, Menu, globalShortcut, ipcMain, clipboard,
  nativeImage, dialog, shell, screen,
} = require('electron');
const path = require('path');
const fs = require('fs');

const settings = require('./settings');
const history = require('./history');
const capture = require('./capture');
const ocr = require('./ocr');
const mcp = require('./mcp');
const windows = require('./windows');
const theme = require('./theme');
const i18n = require('./i18n');
const translate = require('./translate');
const { t } = i18n;

const TEST_MODE = process.env.SHOTIK_TEST === '1' || process.argv.includes('--test');
const GHOST = process.env.SHOTIK_GHOST === '1'; // invisible testing: no hotkeys, off-screen windows

// Test instances live in their own userData: separate settings/history AND a
// separate single-instance lock, so tests can run next to the real app.
if (TEST_MODE) {
  app.setPath('userData', path.join(app.getPath('appData'), 'Shotik-Test'));
}
let tray = null;
let hotkeyErrors = [];
let lastProcessed = null; // for tests / debugging

// ---------- single instance ----------
const gotLock = app.requestSingleInstanceLock();
if (!gotLock) {
  app.quit();
} else {
  app.on('second-instance', (_e, argv) => {
    const mode = cliCaptureMode(argv);
    if (mode === 'region') triggerRegion();
    else if (mode === 'full') triggerFull();
    else if (mode === 'repeat') triggerRepeat();
    else if (mode === 'text') triggerText();
    else windows.createMainWindow({ show: true });
  });
}

function cliCaptureMode(argv) {
  const i = argv.findIndex((a) => a === '--capture' || a.startsWith('--capture='));
  if (i === -1) return null;
  const a = argv[i];
  return a.includes('=') ? a.split('=')[1] : argv[i + 1] || 'region';
}

// ---------- core action processing ----------
function copyToClipboard(pngBuffer, filePath) {
  const img = nativeImage.createFromBuffer(pngBuffer);
  const st = settings.get();
  if (st.smartClipboard && filePath) {
    // Both formats at once: terminals paste the path, image apps paste the picture.
    clipboard.write({ image: img, text: filePath });
  } else {
    clipboard.writeImage(img);
  }
}

async function handleOverlayResult(res) {
  if (!res) return null; // cancelled or busy
  const st = settings.get();

  // remember region for "repeat last" hotkey
  if (res.displayId && res.rectPhys) {
    capture.setLastRegion(Number(res.displayId), {
      x: res.rectPhys.x, y: res.rectPhys.y,
      width: res.rectPhys.w, height: res.rectPhys.h,
    });
  }

  let entry = null;
  switch (res.action) {
    case 'copy': {
      if (st.autoSave) entry = history.savePng(res.png, { source: 'region' });
      copyToClipboard(res.png, entry ? entry.file : null);
      if (st.showToast) windows.showToast({
        kind: 'image', title: t('copiedTitle'),
        body: entry ? path.basename(entry.file) : t('noSave'),
        thumb: entry ? entry.thumb : null, file: entry ? entry.file : null,
      });
      break;
    }
    case 'claude': {
      entry = history.savePng(res.png, { source: 'region' });
      clipboard.write({ image: nativeImage.createFromBuffer(res.png), text: entry.file });
      if (st.showToast) windows.showToast({
        kind: 'claude', title: t('claudeTitle'),
        body: t('claudeBody'),
        thumb: entry.thumb, file: entry.file,
      });
      break;
    }
    case 'save': {
      const r = await dialog.showSaveDialog({
        defaultPath: path.join(st.saveDir, 'Shot.png'),
        filters: [{ name: 'PNG', extensions: ['png'] }],
      });
      if (!r.canceled && r.filePath) {
        fs.writeFileSync(r.filePath, res.png);
        copyToClipboard(res.png, r.filePath);
        if (st.showToast) windows.showToast({ kind: 'image', title: t('savedTitle'), body: r.filePath, file: r.filePath });
        entry = { file: r.filePath };
      }
      break;
    }
    case 'pin': {
      entry = st.autoSave ? history.savePng(res.png, { source: 'pin' }) : null;
      let file = entry ? entry.file : path.join(app.getPath('temp'), `shotik-pin-${Date.now()}.png`);
      if (!entry) fs.writeFileSync(file, res.png);
      if (res.rectCss && res.displayId) {
        const d = capture.displayById(Number(res.displayId));
        windows.createPin({
          file,
          x: d.bounds.x + res.rectCss.x,
          y: d.bounds.y + res.rectCss.y,
          width: res.rectCss.w,
          height: res.rectCss.h,
        });
      }
      break;
    }
    case 'ocr': {
      try {
        const text = await ocr.recognize(res.png);
        if (text) {
          clipboard.writeText(text);
          windows.showToast({ kind: 'text', title: t('ocrDoneTitle'), body: text.slice(0, 120) });
        } else {
          windows.showToast({ kind: 'text', title: t('ocrTitle'), body: t('ocrNoText') });
        }
      } catch (e) {
        windows.showToast({ kind: 'text', title: t('ocrFailTitle'), body: String(e.message || e).slice(0, 120) });
      }
      break;
    }
    case 'copytext': {
      if (res.text) {
        clipboard.writeText(res.text);
        if (st.showToast) windows.showToast({ kind: 'text', title: t('ocrDoneTitle'), body: res.text.slice(0, 120) });
      }
      break;
    }
  }
  if (entry) windows.sendToMain('history:changed');
  lastProcessed = { action: res.action, file: entry ? entry.file : null, ts: Date.now() };
  return lastProcessed;
}

// ---------- capture triggers ----------
// macOS needs the Screen Recording permission; explain instead of failing silently.
function ensureScreenPermission() {
  if (process.platform !== 'darwin') return true;
  try {
    const { systemPreferences } = require('electron');
    if (systemPreferences.getMediaAccessStatus('screen') === 'granted') return true;
  } catch (_) { return true; }
  dialog.showMessageBox({
    type: 'info',
    message: t('permTitle'),
    detail: t('permBody'),
  });
  return false;
}

async function triggerRegion(opts = {}) {
  if (!ensureScreenPermission()) return null;
  const main = windows.getMainWindow();
  if (opts.hideMain && main && main.isVisible()) {
    main.hide();
    await new Promise((r) => setTimeout(r, 180));
  }
  const res = await capture.startOverlaySession();
  if (res !== undefined) await handleOverlayResult(res);
  return lastProcessed;
}

// Live Text: select an area, then extract/translate the text inside it.
async function triggerText(opts = {}) {
  if (!ensureScreenPermission()) return null;
  const main = windows.getMainWindow();
  if (opts.hideMain && main && main.isVisible()) {
    main.hide();
    await new Promise((r) => setTimeout(r, 180));
  }
  const res = await capture.startOverlaySession({ forText: true });
  if (res !== undefined) await handleOverlayResult(res);
  return lastProcessed;
}

async function triggerFull() {
  if (!ensureScreenPermission()) return null;
  const st = settings.get();
  const { png } = await capture.captureFullscreen();
  const entry = st.autoSave ? history.savePng(png, { source: 'fullscreen' }) : null;
  copyToClipboard(png, entry ? entry.file : null);
  if (st.showToast) windows.showToast({
    kind: 'image', title: t('screenCopied'),
    body: entry ? path.basename(entry.file) : t('noSave'),
    thumb: entry ? entry.thumb : null, file: entry ? entry.file : null,
  });
  if (entry) windows.sendToMain('history:changed');
  lastProcessed = { action: 'fullscreen', file: entry ? entry.file : null, ts: Date.now() };
  return lastProcessed;
}

async function triggerRepeat() {
  const st = settings.get();
  const r = await capture.captureLastRegion();
  if (!r) {
    windows.showToast({ kind: 'text', title: t('noLastTitle'), body: t('noLastBody', settings.get().hotkeys.region || 'PrtSc') });
    return null;
  }
  const entry = st.autoSave ? history.savePng(r.png, { source: 'repeat' }) : null;
  copyToClipboard(r.png, entry ? entry.file : null);
  if (st.showToast) windows.showToast({
    kind: 'repeat', title: t('repeatTitle'),
    body: entry ? path.basename(entry.file) : t('copiedTitle'),
    thumb: entry ? entry.thumb : null, file: entry ? entry.file : null,
  });
  if (entry) windows.sendToMain('history:changed');
  lastProcessed = { action: 'repeat', file: entry ? entry.file : null, ts: Date.now() };
  return lastProcessed;
}

// ---------- hotkeys ----------
function registerHotkeys() {
  globalShortcut.unregisterAll();
  hotkeyErrors = [];
  if (GHOST) return; // never grab the user's keys during invisible testing
  const hk = settings.get().hotkeys;
  const map = [
    ['region', hk.region, () => triggerRegion()],
    ['fullscreen', hk.fullscreen, () => triggerFull()],
    ['repeatLast', hk.repeatLast, () => triggerRepeat()],
    ['textGrab', hk.textGrab, () => triggerText()],
  ];
  for (const [name, acc, fn] of map) {
    if (!acc) continue;
    try {
      if (!globalShortcut.register(acc, fn)) hotkeyErrors.push({ name, acc });
    } catch (_) {
      hotkeyErrors.push({ name, acc });
    }
  }
}

// ---------- tray ----------
function buildTrayMenu() {
  const hk = settings.get().hotkeys;
  return Menu.buildFromTemplate([
    { label: t('trayRegion'), sublabel: hk.region, click: () => triggerRegion() },
    { label: t('trayFull'), sublabel: hk.fullscreen, click: () => triggerFull() },
    { label: t('trayRepeat'), sublabel: hk.repeatLast, click: () => triggerRepeat() },
    { label: t('trayText'), sublabel: hk.textGrab, click: () => triggerText() },
    { type: 'separator' },
    { label: t('trayOpen'), click: () => windows.createMainWindow({ show: true }) },
    { type: 'separator' },
    { label: t('trayQuit'), click: () => { global.__shotikQuitting = true; app.quit(); } },
  ]);
}

function createTray() {
  let icon;
  if (process.platform === 'darwin') {
    icon = nativeImage.createFromPath(path.join(__dirname, '..', '..', 'assets', 'trayTemplate.png'));
    icon.setTemplateImage(true); // adapts to the menu bar (light/dark)
  } else {
    icon = nativeImage.createFromPath(path.join(__dirname, '..', '..', 'assets', 'tray.png'));
  }
  tray = new Tray(icon);
  tray.setToolTip(t('trayTooltip'));
  tray.setContextMenu(buildTrayMenu());
  tray.on('click', () => windows.createMainWindow({ show: true }));
}

function refreshTray() {
  if (!tray) return;
  tray.setToolTip(t('trayTooltip'));
  tray.setContextMenu(buildTrayMenu());
}

// ---------- MCP tools ----------
function imageContent(pngBuffer, { fullRes = false } = {}) {
  let img = nativeImage.createFromBuffer(pngBuffer);
  const st = settings.get();
  if (st.downscaleForAI && !fullRes) {
    const { width, height } = img.getSize();
    const maxDim = Math.max(width, height);
    if (maxDim > 1600) {
      const k = 1600 / maxDim;
      img = img.resize({ width: Math.round(width * k), height: Math.round(height * k), quality: 'best' });
    }
  }
  return { type: 'image', data: img.toPNG().toString('base64'), mimeType: 'image/png' };
}

function registerMcpTools() {
  mcp.setTools({
    take_screenshot: {
      description: 'Take a screenshot of the user\'s screen right now and return it as an image. Use display_id from list_displays for a specific monitor (defaults to the primary one). Set full_resolution=true only when you need to read small text.',
      inputSchema: {
        type: 'object',
        properties: {
          display_id: { type: 'number', description: 'Display id from list_displays (optional)' },
          full_resolution: { type: 'boolean', description: 'Return native resolution instead of the downscaled version' },
        },
      },
      handler: async (args) => {
        const { png } = await capture.captureFullscreen(args.display_id || null);
        const entry = history.savePng(png, { source: 'mcp' });
        windows.sendToMain('history:changed');
        return [imageContent(png, { fullRes: !!args.full_resolution }),
          { type: 'text', text: `Saved to ${entry.file} (${entry.width}x${entry.height}px)` }];
      },
    },
    take_screenshot_region: {
      description: 'Screenshot a specific rectangle of the screen (coordinates in device-independent pixels, as reported by list_displays).',
      inputSchema: {
        type: 'object',
        properties: {
          x: { type: 'number' }, y: { type: 'number' },
          width: { type: 'number' }, height: { type: 'number' },
          display_id: { type: 'number', description: 'Display id (optional, defaults to primary)' },
          full_resolution: { type: 'boolean' },
        },
        required: ['x', 'y', 'width', 'height'],
      },
      handler: async (args) => {
        const { png } = await capture.captureRegion(args.display_id || null, args);
        return [imageContent(png, { fullRes: !!args.full_resolution })];
      },
    },
    ask_user_to_select_region: {
      description: 'Open the interactive selection overlay so the USER can mark the exact area to show you. Use when you need the user to point at something. Waits up to 2 minutes.',
      inputSchema: { type: 'object', properties: {} },
      handler: async () => {
        if (capture.isOverlayOpen()) throw new Error('Selection overlay is already open');
        const res = await Promise.race([
          capture.startOverlaySession({ forClaude: true }),
          new Promise((r) => setTimeout(() => r('timeout'), 120000)),
        ]);
        if (res === 'timeout') return [{ type: 'text', text: 'Timed out waiting for the user (2 min).' }];
        if (!res) return [{ type: 'text', text: 'The user cancelled the selection.' }];
        await handleOverlayResult(res);
        return [imageContent(res.png)];
      },
    },
    get_last_screenshot: {
      description: 'Return the most recent screenshot the user took with Shotik.',
      inputSchema: { type: 'object', properties: { full_resolution: { type: 'boolean' } } },
      handler: async (args) => {
        const last = history.last();
        if (!last) return [{ type: 'text', text: 'No screenshots in history yet.' }];
        const png = fs.readFileSync(last.file);
        return [imageContent(png, { fullRes: !!args.full_resolution }),
          { type: 'text', text: `${last.file} (${last.width}x${last.height}px, taken ${new Date(last.ts).toISOString()})` }];
      },
    },
    list_screenshots: {
      description: 'List recent screenshots from Shotik history (paths, sizes, timestamps).',
      inputSchema: { type: 'object', properties: { limit: { type: 'number', description: 'Max entries, default 10' } } },
      handler: async (args) => {
        const items = history.list(Math.min(50, args.limit || 10)).map((it) => ({
          file: it.file, width: it.width, height: it.height,
          taken: new Date(it.ts).toISOString(), source: it.source,
        }));
        return [{ type: 'text', text: JSON.stringify(items, null, 2) }];
      },
    },
    list_displays: {
      description: 'List the user\'s displays: id, bounds (DIP coordinates), scale factor, primary flag.',
      inputSchema: { type: 'object', properties: {} },
      handler: async () => {
        const primaryId = screen.getPrimaryDisplay().id;
        const ds = screen.getAllDisplays().map((d) => ({
          id: d.id, bounds: d.bounds, scaleFactor: d.scaleFactor, primary: d.id === primaryId,
        }));
        return [{ type: 'text', text: JSON.stringify(ds, null, 2) }];
      },
    },
  });
  mcp.setLogListener((entry) => windows.sendToMain('mcp:log', entry));
}

async function startMcp() {
  const st = settings.get();
  if (!st.mcp.enabled) return;
  try {
    await mcp.start(Number(process.env.SHOTIK_PORT) || st.mcp.port);
  } catch (e) {
    console.error('MCP start failed:', e.message);
    windows.showToast({ kind: 'text', title: t('mcpFailTitle'), body: t('mcpFailBody', st.mcp.port, e.message) });
  }
}

// ---------- test endpoints (only in test mode) ----------
function setupTestHandler() {
  if (!TEST_MODE) return;
  mcp.setTestHandler(async (route, body) => {
    switch (route) {
      case 'ping': return 'pong';
      case 'trigger': {
        if (body.mode === 'full') return await triggerFull();
        if (body.mode === 'repeat') return await triggerRepeat();
        if (body.mode === 'text') triggerText(); // don't await — overlay stays open
        else triggerRegion();
        await new Promise((r) => setTimeout(r, 1200));
        return { overlayOpen: capture.isOverlayOpen() };
      }
      case 'state': {
        return {
          overlayOpen: capture.isOverlayOpen(),
          overlayCount: capture.overlayWindows().length,
          overlayBounds: capture.overlayWindows().map((w) => w.getBounds()),
          displays: screen.getAllDisplays().map((d) => ({ id: d.id, bounds: d.bounds, workArea: d.workArea })),
          mainVisible: !!(windows.getMainWindow() && windows.getMainWindow().isVisible()),
          lastProcessed,
          hotkeyErrors,
          mcp: mcp.status(),
        };
      }
      case 'input': {
        const wins = capture.overlayWindows();
        let target = wins[0];
        if (body.displayId) {
          target = wins.find((w) => w.getBounds().x === capture.displayById(body.displayId).bounds.x) || wins[0];
        }
        if (body.target === 'main') target = windows.getMainWindow();
        if (!target) throw new Error('no target window');
        for (const ev of body.events) {
          target.webContents.sendInputEvent(ev);
          await new Promise((r) => setTimeout(r, ev.delay || 30));
        }
        return 'sent';
      }
      case 'clipboard': {
        const img = clipboard.readImage();
        return {
          text: clipboard.readText(),
          hasImage: !img.isEmpty(),
          imageSize: img.isEmpty() ? null : img.getSize(),
          formats: clipboard.availableFormats(),
        };
      }
      case 'screenshot': {
        const { png } = await capture.captureFullscreen(body.displayId || null);
        const out = body.path || path.join(app.getPath('temp'), 'shotik-test-shot.png');
        fs.writeFileSync(out, png);
        return out;
      }
      case 'exec-js': {
        let target;
        if (body.target === 'main') target = windows.getMainWindow();
        else if (body.target === 'toast') target = windows.getToastWindow();
        else if (body.target === 'pin') target = windows.getPinWindows()[body.index || 0];
        else target = capture.overlayWindows()[body.index || 0];
        if (!target) throw new Error('no target window');
        return await target.webContents.executeJavaScript(body.code);
      }
      case 'capture-page': {
        let target;
        if (body.target === 'main') target = windows.getMainWindow();
        else if (body.target === 'toast') target = windows.getToastWindow();
        else if (body.target === 'pin') target = windows.getPinWindows()[body.index || 0];
        else target = capture.overlayWindows()[body.index || 0];
        if (!target) throw new Error('no target window: ' + (body.target || 'overlay'));
        if (!target.isVisible()) target.showInactive();
        const img = await Promise.race([
          target.webContents.capturePage(),
          new Promise((_, rej) => setTimeout(() => rej(new Error('capturePage timeout')), 5000)),
        ]);
        const out = body.path || path.join(app.getPath('temp'), 'shotik-page.png');
        fs.writeFileSync(out, img.toPNG());
        return { path: out, size: img.getSize() };
      }
      case 'last-toast': return windows.getLastToast();
      case 'pins': return windows.getPinWindows().map((w) => ({ bounds: w.getBounds() }));
      case 'set-theme': {
        const { nativeTheme } = require('electron');
        nativeTheme.themeSource = body.mode || 'system';
        return theme.getTheme();
      }
      case 'show-main': { windows.createMainWindow({ show: true }); return 'ok'; }
      case 'quit': { global.__shotikQuitting = true; setTimeout(() => app.quit(), 100); return 'bye'; }
      default: throw new Error('unknown test route: ' + route);
    }
  });
}

// ---------- app window IPC ----------
function setupAppIpc() {
  ipcMain.handle('app:state', () => ({
    settings: settings.get(),
    version: app.getVersion(),
    mcp: mcp.status(),
    mcpLog: mcp.getLog(),
    hotkeyErrors,
    hasLastRegion: capture.hasLastRegion(),
  }));

  ipcMain.handle('settings:set', async (_e, patch) => {
    const before = settings.get();
    const after = settings.set(patch);
    if (JSON.stringify(before.hotkeys) !== JSON.stringify(after.hotkeys)) {
      registerHotkeys();
      refreshTray();
    }
    if (before.mcp.enabled !== after.mcp.enabled || before.mcp.port !== after.mcp.port) {
      await mcp.stop();
      if (after.mcp.enabled) await startMcp();
    }
    if (before.launchAtStartup !== after.launchAtStartup) {
      app.setLoginItemSettings({ openAtLogin: after.launchAtStartup, args: ['--hidden'] });
    }
    if (before.language !== after.language) refreshTray();
    return { settings: settings.get(), mcp: mcp.status(), hotkeyErrors };
  });

  ipcMain.handle('i18n:get', () => ({ lang: i18n.lang(), dict: i18n.dict() }));

  ipcMain.handle('settings:choose-dir', async () => {
    const r = await dialog.showOpenDialog(windows.getMainWindow(), {
      properties: ['openDirectory', 'createDirectory'],
      defaultPath: settings.get().saveDir,
    });
    if (r.canceled || !r.filePaths[0]) return null;
    settings.set({ saveDir: r.filePaths[0] });
    return r.filePaths[0];
  });

  ipcMain.handle('history:list', (_e, limit) => history.list(limit || 200));
  ipcMain.handle('history:remove', (_e, id) => { history.remove(id); return history.list(200); });
  ipcMain.handle('history:clear', (_e, opts) => { history.clearAll(opts || {}); return []; });
  ipcMain.handle('history:copy', (_e, id) => {
    const it = history.get(id);
    if (!it) return false;
    copyToClipboard(fs.readFileSync(it.file), it.file);
    windows.showToast({ kind: 'image', title: 'Скопировано в буфер', body: path.basename(it.file), thumb: it.thumb, file: it.file });
    return true;
  });
  ipcMain.handle('history:copy-path', (_e, id) => {
    const it = history.get(id);
    if (!it) return false;
    clipboard.writeText(it.file);
    return true;
  });
  ipcMain.handle('history:pin', (_e, id) => {
    const it = history.get(id);
    if (!it) return false;
    const d = screen.getPrimaryDisplay();
    const w = Math.min(Math.round(it.width / d.scaleFactor), Math.round(d.workArea.width * 0.6));
    const h = Math.round(w * it.height / it.width);
    windows.createPin({
      file: it.file,
      x: d.workArea.x + Math.round((d.workArea.width - w) / 2),
      y: d.workArea.y + Math.round((d.workArea.height - h) / 2),
      width: w, height: h,
    });
    return true;
  });
  ipcMain.handle('history:reveal', (_e, id) => { const it = history.get(id); if (it) shell.showItemInFolder(it.file); });
  ipcMain.handle('history:open', (_e, id) => { const it = history.get(id); if (it) shell.openPath(it.file); });
  ipcMain.handle('history:ocr', async (_e, id) => {
    const it = history.get(id);
    if (!it) return null;
    const text = await ocr.recognize(fs.readFileSync(it.file));
    if (text) clipboard.writeText(text);
    return text;
  });

  // Live Text overlay IPC
  ipcMain.handle('overlay:ocr-boxes', async (_e, pngArray) => {
    try { return await ocr.recognizeBoxes(Buffer.from(pngArray)); } catch (_) { return { lines: [] }; }
  });
  ipcMain.handle('overlay:translate', async (_e, text) => {
    const res = await translate.translate(text);
    return { ...res, target: (settings.get().translate || {}).target };
  });

  ipcMain.handle('capture:region', () => triggerRegion({ hideMain: true }));
  ipcMain.handle('capture:text', () => triggerText({ hideMain: true }));
  ipcMain.handle('capture:full', async () => {
    const main = windows.getMainWindow();
    if (main && main.isVisible()) { main.hide(); await new Promise((r) => setTimeout(r, 180)); }
    return triggerFull();
  });
  ipcMain.handle('capture:repeat', () => triggerRepeat());

  ipcMain.handle('theme:get', () => theme.getTheme());

  ipcMain.handle('app:open-external', (_e, url) => {
    if (/^https?:\/\//.test(url)) shell.openExternal(url);
  });
  ipcMain.handle('app:open-save-dir', () => {
    const dir = settings.get().saveDir;
    fs.mkdirSync(dir, { recursive: true });
    shell.openPath(dir);
  });
}

// ---------- lifecycle ----------
app.on('before-quit', () => { global.__shotikQuitting = true; });
app.on('will-quit', () => globalShortcut.unregisterAll());
app.on('window-all-closed', () => { /* tray app — keep running */ });

app.whenReady().then(async () => {
  app.setAppUserModelId('dev.shotik.app');
  settings.get();
  registerMcpTools();
  setupTestHandler();
  setupAppIpc();
  createTray();
  registerHotkeys();
  await startMcp();
  capture.events.on('color-copied', (hex) => {
    clipboard.writeText(hex);
    windows.showToast({ kind: 'color', title: t('colorTitle'), body: hex, color: hex });
  });
  theme.watch((t) => windows.applyTheme(t));

  const hidden = process.argv.includes('--hidden');
  windows.createMainWindow({ show: !hidden });

  const mode = cliCaptureMode(process.argv);
  if (mode === 'region') setTimeout(() => triggerRegion(), 400);
  else if (mode === 'full') setTimeout(() => triggerFull(), 400);
  else if (mode === 'repeat') setTimeout(() => triggerRepeat(), 400);
  else if (mode === 'text') setTimeout(() => triggerText(), 400);
});
