'use strict';
// Per-window capture for the MCP tools (list_windows / capture_window).
// Unlike the screen tools this reads the WINDOW's own surface, so it works for
// windows that are covered by other windows, off-screen or on any monitor.
//
// Windows: enumerate via windows-enum.ps1 -Detailed (hwnd/title/app/bounds),
//   capture via capture-window.ps1 (PrintWindow PW_RENDERFULLCONTENT — proven
//   to read DWM-composited content regardless of occlusion). If that yields a
//   blank/uniform image (some GPU-only surfaces), fall back to Electron's
//   desktopCapturer window thumbnail matched by HWND.
// macOS: desktopCapturer window sources only (CGWindowList handles occlusion).
const { desktopCapturer, screen, app, nativeImage } = require('electron');
const { execFile } = require('child_process');
const fs = require('fs');
const path = require('path');

const TEST_MODE = process.env.SHOTIK_TEST === '1';

const psPath = (name) => path.join(__dirname, name)
  .replace('app.asar' + path.sep, 'app.asar.unpacked' + path.sep);

function runPs(args, timeout = 8000) {
  return new Promise((resolve, reject) => {
    execFile('powershell.exe',
      ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', ...args],
      { timeout, windowsHide: true, maxBuffer: 8 * 1024 * 1024 },
      (err, stdout, stderr) => {
        if (err) return reject(new Error(String(stderr || err.message).trim().slice(0, 300)));
        resolve(String(stdout).trim());
      });
  });
}

// Detailed top-level window list (physical px, z-order top first). In test mode
// Shotik's own windows are included so ghost tests can capture themselves.
async function enumDetailed() {
  const out = await runPs(['-File', psPath('windows-enum.ps1'),
    '-ExcludePid', String(TEST_MODE ? 0 : process.pid), '-Detailed']);
  try { return JSON.parse(out || '[]'); } catch (_) { throw new Error('window enumeration returned malformed data'); }
}

const physToDip = (r) => {
  const rect = { x: r.x, y: r.y, width: r.w, height: r.h };
  try { return screen.screenToDipRect(null, rect); } catch (_) { return rect; }
};

// Public: cross-platform window list for the MCP tool.
async function listWindows() {
  if (process.platform === 'win32') {
    return (await enumDetailed()).map((w) => {
      // A minimized window has no true on-screen bounds — GetWindowPlacement's
      // restore rect is in WORKSPACE coords (shifts by the taskbar thickness
      // when it's not at the bottom), so reporting it as gospel would be wrong.
      if (w.min) return { id: w.hwnd, title: w.title, app: w.app || null, bounds: null, display_id: null, minimized: true };
      const dip = physToDip(w);
      let displayId = null;
      try { displayId = screen.getDisplayMatching(dip).id; } catch (_) {}
      return {
        id: w.hwnd, title: w.title, app: w.app || null,
        bounds: { x: Math.round(dip.x), y: Math.round(dip.y), width: Math.round(dip.width), height: Math.round(dip.height) },
        display_id: displayId, minimized: false,
      };
    });
  }
  // macOS (experimental): titles only — CGWindowList bounds aren't exposed here
  ensureMacPermission();
  const sources = await desktopCapturer.getSources({ types: ['window'], thumbnailSize: { width: 0, height: 0 } });
  return sources.map((s) => ({ id: s.id, title: s.name, app: null, bounds: null, display_id: null, minimized: false }));
}

// macOS silently returns empty/black window sources without the Screen
// Recording permission — turn that into an explanation the agent can act on.
function ensureMacPermission() {
  if (process.platform !== 'darwin') return;
  try {
    const { systemPreferences } = require('electron');
    if (systemPreferences.getMediaAccessStatus('screen') !== 'granted') {
      throw new Error('macOS Screen Recording permission is not granted — ask the user to enable it for Shotik in System Settings → Privacy & Security → Screen Recording.');
    }
  } catch (e) {
    if (/Screen Recording/.test(e.message)) throw e; // our own message
  }
}

// A capture that came back as one flat color is a failed capture in disguise
// (GPU surface GDI couldn't read). Sampled on a 16x16 grid — cheap.
function looksBlank(img) {
  if (!img || img.isEmpty()) return true;
  const { width, height } = img.getSize();
  if (width < 2 || height < 2) return true;
  const bmp = img.toBitmap();
  const r0 = bmp[0], g0 = bmp[1], b0 = bmp[2];
  const stepX = Math.max(1, Math.floor(width / 16));
  const stepY = Math.max(1, Math.floor(height / 16));
  for (let y = 0; y < height; y += stepY) {
    for (let x = 0; x < width; x += stepX) {
      const i = (y * width + x) * 4;
      if (Math.abs(bmp[i] - r0) > 3 || Math.abs(bmp[i + 1] - g0) > 3 || Math.abs(bmp[i + 2] - b0) > 3) return false;
    }
  }
  return true;
}

let capSeq = 0; // pid + counter -> collision-proof temp names under concurrent calls

async function printWindowPng(hwnd) {
  const out = path.join(app.getPath('temp'), `shotik-wincap-${process.pid}-${++capSeq}-${hwnd}.png`);
  try {
    await runPs(['-File', psPath('capture-window.ps1'), '-Hwnd', String(hwnd), '-OutFile', out]);
    const img = nativeImage.createFromPath(out);
    if (img.isEmpty()) throw new Error('PrintWindow produced an empty image');
    return img;
  } finally {
    setTimeout(() => { try { fs.unlinkSync(out); } catch (_) {} }, 500);
  }
}

// Electron window-source id is "window:<id>:<seq>"; on Windows <id> is the HWND.
async function thumbnailByHwnd(hwnd, physW, physH) {
  const sources = await desktopCapturer.getSources({
    types: ['window'], thumbnailSize: { width: physW, height: physH }, fetchWindowIcons: false,
  });
  const src = sources.find((s) => {
    const m = /^window:(\d+):/.exec(s.id);
    return m && Number(m[1]) === Number(hwnd);
  });
  return src ? src.thumbnail : null;
}

// exact > startsWith > substring, case-insensitive
function scoreTitle(winTitle, query) {
  const t = winTitle.toLowerCase(), q = query.toLowerCase();
  if (t === q) return 3;
  if (t.startsWith(q)) return 2;
  if (t.includes(q)) return 1;
  return 0;
}

// Public: capture one window -> { png, title, app, id, method, otherMatches }
async function captureWindow({ windowId = null, title = null } = {}) {
  if (windowId == null && !title) throw new Error('Pass window_id (from list_windows) or title (substring of the window title)');

  if (process.platform !== 'win32') {
    // macOS path: match a desktopCapturer window source directly
    ensureMacPermission();
    const sources = await desktopCapturer.getSources({
      types: ['window'], thumbnailSize: { width: 2560, height: 1600 }, fetchWindowIcons: false,
    });
    let src = null;
    if (windowId != null) src = sources.find((s) => s.id === String(windowId));
    else {
      const scored = sources.map((s) => ({ s, sc: scoreTitle(s.name || '', title) }))
        .filter((x) => x.sc > 0).sort((a, b) => b.sc - a.sc);
      src = scored.length ? scored[0].s : null;
    }
    if (!src) throw new Error(`No window matches ${windowId != null ? `id ${windowId}` : `title "${title}"`}. Use list_windows.`);
    if (src.thumbnail.isEmpty()) throw new Error(`Window "${src.name}" captured as an empty image`);
    return { png: src.thumbnail.toPNG(), title: src.name, app: null, id: src.id, method: 'desktopCapturer', otherMatches: [], uniform: looksBlank(src.thumbnail) };
  }

  const wins = await enumDetailed();
  if (!wins.length) throw new Error('Could not enumerate windows');
  let target = null;
  let otherMatches = [];
  if (windowId != null) {
    target = wins.find((w) => Number(w.hwnd) === Number(windowId));
    if (!target) throw new Error(`No window with id ${windowId}. Use list_windows for current ids.`);
    // Windows recycles hwnd values: a stale id can silently point at an
    // unrelated window. When the caller also passed a title, use it as a guard.
    if (title && scoreTitle(target.title || '', title) === 0) {
      throw new Error(`Window id ${windowId} now belongs to "${target.title}", which does not match title "${title}" — ` +
        'the id is stale (window ids get recycled). Call list_windows again for a fresh id.');
    }
  } else {
    const scored = wins.map((w) => ({ w, sc: scoreTitle(w.title || '', title) }))
      .filter((x) => x.sc > 0).sort((a, b) => b.sc - a.sc);
    if (!scored.length) throw new Error(`No window matches title "${title}". Use list_windows for exact titles.`);
    target = scored[0].w;
    otherMatches = scored.slice(1, 6).map((x) => x.w.title);
  }
  if (target.min) {
    throw new Error(`Window "${target.title}" is minimized — its surface has no pixels while iconic. Ask the user to restore it, then retry.`);
  }

  let img = null, method = 'printwindow', firstError = null;
  if (process.env.SHOTIK_WINCAP_FAIL === '1') firstError = new Error('forced failure (test)');
  else { try { img = await printWindowPng(target.hwnd); } catch (e) { firstError = e; } }
  // A uniform image usually means GDI couldn't read a GPU surface — but it can
  // also be a genuinely blank window, so keep it as a last resort instead of
  // erroring on correct captures.
  const primaryBlank = !img || looksBlank(img);
  if (primaryBlank) {
    try {
      const alt = await thumbnailByHwnd(target.hwnd, target.w, target.h);
      if (alt && !alt.isEmpty() && (!looksBlank(alt) || !img)) { img = alt; method = 'desktopCapturer'; }
    } catch (_) {}
  }
  if (!img || img.isEmpty()) {
    throw new Error(`Could not capture "${target.title}"${firstError ? ` (${firstError.message})` : ''}. ` +
      'The window may use a GPU-only surface; take_screenshot of its display is the fallback.');
  }
  const uniform = looksBlank(img);
  return { png: img.toPNG(), title: target.title, app: target.app || null, id: target.hwnd, method, otherMatches, uniform };
}

module.exports = { listWindows, captureWindow, looksBlank };
