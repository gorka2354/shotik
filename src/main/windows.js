'use strict';
// Main app window, toast notifications, pinned screenshot windows.
const { BrowserWindow, screen, ipcMain, clipboard, nativeImage, dialog, Menu, shell, nativeTheme } = require('electron');
const path = require('path');
const fs = require('fs');

const winBg = () => (nativeTheme.shouldUseDarkColors ? '#202020' : '#f3f3f3');
const symbolColor = () => (nativeTheme.shouldUseDarkColors ? '#cfcfcf' : '#1b1b1b');

const GHOST = process.env.SHOTIK_GHOST === '1';
const GHOST_OFFSET = 20000;

let mainWin = null;
let lastToast = null; // recorded for tests
let toastWin = null;
let toastTimer = null;
const pins = new Map(); // id -> { win, file, baseW, baseH, zoom }
let pinSeq = 0;

// ---------- main window ----------
function createMainWindow({ show = true } = {}) {
  if (mainWin && !mainWin.isDestroyed()) {
    if (show) { mainWin.show(); mainWin.focus(); }
    return mainWin;
  }
  mainWin = new BrowserWindow({
    width: 1120, height: 740, minWidth: 880, minHeight: 600,
    ...(GHOST ? { x: GHOST_OFFSET, y: 60 } : {}),
    show: false, frame: false, backgroundColor: winBg(),
    titleBarStyle: process.platform === 'darwin' ? 'hiddenInset' : 'hidden',
    ...(process.platform === 'darwin'
      ? { trafficLightPosition: { x: 16, y: 14 } }
      // solid color (matching the window bg): WCO derives the minimize/maximize
      // hover tint from it, with a transparent color the hover is invisible
      : { titleBarOverlay: { color: winBg(), symbolColor: symbolColor(), height: 44 } }),
    icon: path.join(__dirname, '..', '..', 'assets', 'icon.png'),
    webPreferences: {
      preload: path.join(__dirname, '..', 'app', 'preload.js'),
      contextIsolation: true, nodeIntegration: false, spellcheck: false,
      backgroundThrottling: false,
    },
  });
  mainWin.removeMenu();
  mainWin.loadFile(path.join(__dirname, '..', 'app', 'index.html'));
  mainWin.once('ready-to-show', () => { if (show) (GHOST ? mainWin.showInactive() : mainWin.show()); });
  // Closing hides to tray; app quits via tray menu.
  mainWin.on('close', (e) => {
    if (!global.__shotikQuitting) { e.preventDefault(); mainWin.hide(); }
  });
  return mainWin;
}

function getMainWindow() { return mainWin && !mainWin.isDestroyed() ? mainWin : null; }

function sendToMain(channel, payload) {
  const w = getMainWindow();
  if (w) w.webContents.send(channel, payload);
}

// ---------- toast ----------
const TOAST_W = 380, TOAST_H = 92;

function ensureToast() {
  if (toastWin && !toastWin.isDestroyed()) return toastWin;
  toastWin = new BrowserWindow({
    width: TOAST_W, height: TOAST_H, show: false,
    frame: false, transparent: true, resizable: false, movable: false,
    alwaysOnTop: true, skipTaskbar: true, focusable: false, hasShadow: false,
    webPreferences: {
      preload: path.join(__dirname, '..', 'toast', 'preload.js'),
      contextIsolation: true, nodeIntegration: false, backgroundThrottling: false,
    },
  });
  toastWin.loadFile(path.join(__dirname, '..', 'toast', 'toast.html'));
  toastWin.setAlwaysOnTop(true, 'screen-saver');
  return toastWin;
}

// showToast({title, body, thumb (file path), file (target to open), kind})
function showToast(data) {
  lastToast = { ...data, ts: Date.now() };
  const w = ensureToast();
  const area = screen.getDisplayNearestPoint(screen.getCursorScreenPoint()).workArea;
  w.setBounds({
    x: area.x + area.width - TOAST_W - 16 + (GHOST ? GHOST_OFFSET : 0),
    y: area.y + area.height - TOAST_H - 16,
    width: TOAST_W, height: TOAST_H,
  });
  const show = () => {
    w.webContents.send('toast:data', data);
    w.showInactive();
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { if (toastWin && !toastWin.isDestroyed()) toastWin.hide(); }, 4200);
  };
  if (w.webContents.isLoading()) w.webContents.once('did-finish-load', show);
  else show();
}

ipcMain.on('toast:close', () => { if (toastWin && !toastWin.isDestroyed()) toastWin.hide(); });
ipcMain.on('toast:open-file', (_e, file) => {
  if (toastWin && !toastWin.isDestroyed()) toastWin.hide();
  if (file && fs.existsSync(file)) shell.showItemInFolder(file);
});

// ---------- pinned screenshots ----------
// createPin({file, x, y, width, height}) — DIP screen coords where the shot was taken.
function createPin({ file, x, y, width, height }) {
  const id = 'pin' + (++pinSeq);
  const win = new BrowserWindow({
    x: Math.round(x) + (GHOST ? GHOST_OFFSET : 0), y: Math.round(y),
    width: Math.max(40, Math.round(width)), height: Math.max(40, Math.round(height)),
    frame: false, transparent: false, backgroundColor: '#11141a',
    alwaysOnTop: true, skipTaskbar: true, resizable: false, hasShadow: true,
    roundedCorners: true, minimizable: false, maximizable: false, fullscreenable: false,
    webPreferences: {
      preload: path.join(__dirname, '..', 'pin', 'preload.js'),
      contextIsolation: true, nodeIntegration: false, backgroundThrottling: false,
    },
  });
  win.removeMenu();
  win.setAlwaysOnTop(true, 'floating');
  win.loadFile(path.join(__dirname, '..', 'pin', 'pin.html'), {
    query: { img: 'file:///' + file.replace(/\\/g, '/'), id },
  });
  pins.set(id, { win, file, baseW: Math.round(width), baseH: Math.round(height), zoom: 1 });
  win.on('closed', () => pins.delete(id));
  return id;
}

function pinZoom(id, dir) {
  const p = pins.get(id);
  if (!p || p.win.isDestroyed()) return;
  const next = Math.min(5, Math.max(0.2, p.zoom * (dir > 0 ? 1.1 : 1 / 1.1)));
  if (Math.abs(next - p.zoom) < 0.001) return;
  const b = p.win.getBounds();
  const w = Math.max(40, Math.round(p.baseW * next));
  const h = Math.max(40, Math.round(p.baseH * next));
  // zoom around window center
  p.win.setBounds({
    x: Math.round(b.x + (b.width - w) / 2),
    y: Math.round(b.y + (b.height - h) / 2),
    width: w, height: h,
  });
  p.zoom = next;
  p.win.webContents.send('pin:zoom-level', Math.round(next * 100));
}

ipcMain.on('pin:zoom', (_e, { id, dir }) => pinZoom(id, dir));
ipcMain.on('pin:close', (_e, id) => { const p = pins.get(id); if (p && !p.win.isDestroyed()) p.win.close(); });
ipcMain.on('pin:reset', (_e, id) => {
  const p = pins.get(id);
  if (!p || p.win.isDestroyed()) return;
  const b = p.win.getBounds();
  p.win.setBounds({ x: b.x, y: b.y, width: p.baseW, height: p.baseH });
  p.zoom = 1;
});
ipcMain.on('pin:menu', (e, id) => {
  const p = pins.get(id);
  if (!p || p.win.isDestroyed()) return;
  const { t } = require('./i18n');
  const menu = Menu.buildFromTemplate([
    { label: t('pinCopy'), click: () => clipboard.writeImage(nativeImage.createFromPath(p.file)) },
    { label: t('pinSaveAs'), click: async () => {
        const r = await dialog.showSaveDialog(p.win, { defaultPath: path.basename(p.file), filters: [{ name: 'PNG', extensions: ['png'] }] });
        if (!r.canceled && r.filePath) fs.copyFileSync(p.file, r.filePath);
      } },
    { label: t('pinReveal'), click: () => shell.showItemInFolder(p.file) },
    { type: 'separator' },
    ...[100, 85, 70, 50].map((op) => ({
      label: t('pinOpacity', op), type: 'radio', checked: Math.round(p.win.getOpacity() * 100) === op,
      click: () => p.win.setOpacity(op / 100),
    })),
    { type: 'separator' },
    { label: t('pinReset'), click: () => ipcMain.emit('pin:reset', null, id) },
    { label: t('pinClose'), click: () => p.win.close() },
  ]);
  menu.popup({ window: p.win });
});

function closeAllPins() { for (const { win } of pins.values()) { try { win.close(); } catch (_) {} } }

// ---------- translation popup (global select → translate) ----------
let tpWin = null;
let tpData = null;   // latest payload (so did-finish-load sends the current state)
let tpReady = false;
const TP_W = 380;

function sendTpData() {
  if (tpReady && tpWin && !tpWin.isDestroyed() && tpData) tpWin.webContents.send('tp:data', tpData);
}

function showTranslatePopup(data) {
  tpData = data;
  const pt = screen.getCursorScreenPoint();
  const area = screen.getDisplayNearestPoint(pt).workArea;
  let x = pt.x + 12, y = pt.y + 16;
  if (x + TP_W > area.x + area.width) x = area.x + area.width - TP_W - 8;
  if (x < area.x) x = area.x + 8;
  if (y > area.y + area.height - 160) y = area.y + area.height - 240;

  if (tpWin && !tpWin.isDestroyed()) {
    tpWin.setBounds({ x: Math.round(x) + (GHOST ? GHOST_OFFSET : 0), y: Math.round(y), width: TP_W, height: 220 });
    if (GHOST) tpWin.showInactive(); else tpWin.show();
    sendTpData();
    return;
  }
  tpReady = false;
  tpWin = new BrowserWindow({
    x: Math.round(x) + (GHOST ? GHOST_OFFSET : 0), y: Math.round(y),
    width: TP_W, height: 220, show: false,
    frame: false, transparent: true, resizable: false, skipTaskbar: true,
    alwaysOnTop: true, minimizable: false, maximizable: false, hasShadow: false,
    webPreferences: {
      preload: path.join(__dirname, '..', 'translate-popup', 'preload.js'),
      contextIsolation: true, nodeIntegration: false, backgroundThrottling: false,
    },
  });
  tpWin.removeMenu();
  tpWin.setAlwaysOnTop(true, 'screen-saver');
  tpWin.loadFile(path.join(__dirname, '..', 'translate-popup', 'popup.html'));
  tpWin.webContents.once('did-finish-load', () => {
    if (!tpWin || tpWin.isDestroyed()) return;
    tpReady = true;
    if (GHOST) tpWin.showInactive(); else tpWin.show();
    sendTpData();
  });
  tpWin.on('closed', () => { tpWin = null; tpReady = false; });
  // dismiss on click-away (only after it has actually been focused, to avoid
  // an immediate self-dismiss right after show)
  let focusedOnce = false;
  tpWin.on('focus', () => { focusedOnce = true; });
  tpWin.on('blur', () => { if (focusedOnce && tpWin && !tpWin.isDestroyed()) tpWin.hide(); });
}

function updateTranslatePopup(data) {
  tpData = data;
  sendTpData();
}

ipcMain.on('tp:copy', (_e, text) => { if (text) clipboard.writeText(text); if (tpWin && !tpWin.isDestroyed()) tpWin.hide(); });
ipcMain.on('tp:close', () => { if (tpWin && !tpWin.isDestroyed()) tpWin.hide(); });
ipcMain.on('tp:resize', (_e, h) => {
  if (tpWin && !tpWin.isDestroyed()) {
    const b = tpWin.getBounds();
    tpWin.setBounds({ x: b.x, y: b.y, width: TP_W, height: Math.max(120, Math.min(520, Math.round(h))) });
    tpWin.showInactive();
  }
});

// Re-sync native chrome with the OS theme and notify renderers.
function applyTheme(t) {
  if (mainWin && !mainWin.isDestroyed()) {
    mainWin.setBackgroundColor(winBg());
    try { mainWin.setTitleBarOverlay({ color: winBg(), symbolColor: symbolColor(), height: 44 }); } catch (_) {}
    mainWin.webContents.send('theme:changed', t);
  }
  if (toastWin && !toastWin.isDestroyed()) toastWin.webContents.send('theme:changed', t);
  for (const { win } of pins.values()) {
    if (!win.isDestroyed()) win.webContents.send('theme:changed', t);
  }
}

function getLastToast() { return lastToast; }
function getToastWindow() { return toastWin && !toastWin.isDestroyed() ? toastWin : null; }
function getPinWindows() { return [...pins.values()].map((p) => p.win).filter((w) => !w.isDestroyed()); }

function getTranslatePopup() { return tpWin && !tpWin.isDestroyed() ? tpWin : null; }

module.exports = {
  createMainWindow, getMainWindow, sendToMain, showToast, createPin, closeAllPins,
  getLastToast, getToastWindow, getPinWindows, applyTheme,
  showTranslatePopup, updateTranslatePopup, getTranslatePopup,
};
