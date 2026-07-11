'use strict';
// Main app window, toast notifications, pinned screenshot windows.
const { BrowserWindow, screen, ipcMain, clipboard, nativeImage, dialog, Menu, shell, nativeTheme } = require('electron');
const path = require('path');
const fs = require('fs');

const winBg = () => (nativeTheme.shouldUseDarkColors ? '#202020' : '#f3f3f3');
const symbolColor = () => (nativeTheme.shouldUseDarkColors ? '#cfcfcf' : '#1b1b1b');

const GHOST = process.env.SHOTIK_GHOST === '1';
const GHOST_OFFSET = 20000;
// Tests: force the focusable show() path (and blur-dismiss) even in ghost, so the
// real popup focus behaviour can be exercised off-screen. Off = ghost uses showInactive.
const POPUP_REAL = process.env.SHOTIK_POPUP_REAL === '1';
const popupShow = (w) => { if (GHOST && !POPUP_REAL) w.showInactive(); else w.show(); };

// Shared diagnostics file (see main.js blog()) — trace the popup's own show/blur.
function wlog(msg) {
  try {
    const { app } = require('electron');
    fs.appendFileSync(path.join(app.getPath('temp'), 'shotik-bubble.log'),
      new Date().toISOString().slice(11, 23) + ' [win] ' + msg + '\n');
  } catch (_) {}
}

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
let tpHideTimer = null;
const TP_W = 380;
const TP_TTL = Number(process.env.SHOTIK_TP_TTL || 9000); // auto-hide after the result lands

function sendTpData() {
  if (tpReady && tpWin && !tpWin.isDestroyed() && tpData) tpWin.webContents.send('tp:data', tpData);
}

// Auto-hide the popup a while after the result is shown; hovering it cancels the
// countdown (restarted on mouse-leave). We deliberately do NOT dismiss on blur —
// real window focus proved far too flaky (the popup was vanishing before the
// translation even rendered). Esc / × / Copy still close it immediately.
function armPopupHide() {
  clearTimeout(tpHideTimer);
  tpHideTimer = setTimeout(() => { if (tpWin && !tpWin.isDestroyed()) { wlog('popup auto-hide'); tpWin.hide(); } }, TP_TTL);
}

function showTranslatePopup(data) {
  tpData = data;
  wlog(`popup show (${data.loading ? 'loading' : (data.error ? 'error' : 'result')}) reuse=${!!(tpWin && !tpWin.isDestroyed())}`);
  if (data.loading) clearTimeout(tpHideTimer); else armPopupHide(); // never time out mid-load
  const pt = screen.getCursorScreenPoint();
  const area = screen.getDisplayNearestPoint(pt).workArea;
  let x = pt.x + 12, y = pt.y + 16;
  if (x + TP_W > area.x + area.width) x = area.x + area.width - TP_W - 8;
  if (x < area.x) x = area.x + 8;
  if (y > area.y + area.height - 160) y = area.y + area.height - 240;

  if (tpWin && !tpWin.isDestroyed()) {
    tpWin.setBounds({ x: Math.round(x) + (GHOST ? GHOST_OFFSET : 0), y: Math.round(y), width: TP_W, height: 220 });
    popupShow(tpWin);
    tpWin.moveTop();
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
    popupShow(tpWin);
    sendTpData();
  });
  tpWin.on('closed', () => { tpWin = null; tpReady = false; clearTimeout(tpHideTimer); });
}

function updateTranslatePopup(data) {
  tpData = data;
  armPopupHide(); // result/error is on screen now — start the auto-hide countdown
  sendTpData();
}

// ---------- selection bubble (auto-translate on selection) ----------
// A tiny always-on-top button that appears next to a fresh text selection.
// Clicking it (handled in main.js via 'bubble:activate') opens the translation.
let sbWin = null;
let sbTimer = null;
const SB = 48; // window box; the whole window is clickable (see bubble.js)
const SB_TTL = 6000;

function ensureBubble() {
  if (sbWin && !sbWin.isDestroyed()) return sbWin;
  sbWin = new BrowserWindow({
    width: SB, height: SB, show: false,
    frame: false, transparent: true, resizable: false, movable: false,
    // focusable:true so the click is delivered reliably (a non-focusable
    // always-on-top window can silently swallow clicks after another window
    // took focus). We already captured the text via UIA, so taking focus on
    // click is harmless.
    alwaysOnTop: true, skipTaskbar: true, focusable: true, hasShadow: false,
    minimizable: false, maximizable: false, fullscreenable: false,
    webPreferences: {
      preload: path.join(__dirname, '..', 'bubble', 'preload.js'),
      contextIsolation: true, nodeIntegration: false, backgroundThrottling: false,
    },
  });
  sbWin.removeMenu();
  sbWin.setAlwaysOnTop(true, 'screen-saver');
  sbWin.loadFile(path.join(__dirname, '..', 'bubble', 'bubble.html'));
  return sbWin;
}

function armBubbleHide() {
  clearTimeout(sbTimer);
  sbTimer = setTimeout(() => { wlog('bubble auto-hide'); hideSelectionBubble(); }, SB_TTL);
}

// showSelectionBubble({x, y}) — DIP coords near the end of the selection.
function showSelectionBubble({ x, y }) {
  const w = ensureBubble();
  const area = screen.getDisplayNearestPoint({ x: Math.round(x), y: Math.round(y) }).workArea;
  let bx = x + 14, by = y + 16; // offset so the button isn't under the pointer
  if (bx + SB > area.x + area.width) bx = x - SB - 8;
  if (by + SB > area.y + area.height) by = y - SB - 8;
  if (bx < area.x) bx = area.x + 4;
  if (by < area.y) by = area.y + 4;
  w.setBounds({ x: Math.round(bx) + (GHOST ? GHOST_OFFSET : 0), y: Math.round(by), width: SB, height: SB });
  w.showInactive(); // don't steal focus when it merely appears
  w.setAlwaysOnTop(true, 'screen-saver');
  w.moveTop();       // make sure it's above a just-hidden translate popup
  wlog(`bubble show @ ${Math.round(bx)},${Math.round(by)}`);
  armBubbleHide();
}

function hideSelectionBubble() {
  clearTimeout(sbTimer);
  if (sbWin && !sbWin.isDestroyed()) sbWin.hide();
}

function getSelectionBubble() { return sbWin && !sbWin.isDestroyed() ? sbWin : null; }

ipcMain.on('bubble:keepalive', () => armBubbleHide());

ipcMain.on('tp:copy', (_e, text) => { clearTimeout(tpHideTimer); if (text) clipboard.writeText(text); if (tpWin && !tpWin.isDestroyed()) tpWin.hide(); });
ipcMain.on('tp:close', () => { clearTimeout(tpHideTimer); if (tpWin && !tpWin.isDestroyed()) tpWin.hide(); });
ipcMain.on('tp:hover', (_e, over) => { if (over) clearTimeout(tpHideTimer); else armPopupHide(); }); // pause auto-hide while hovering
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
  showSelectionBubble, hideSelectionBubble, getSelectionBubble,
};
