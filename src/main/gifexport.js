'use strict';
// Turns a recorded video into a GIF. A hidden window decodes + samples frames;
// here we quantize to a single global palette and encode with gifenc.
const { BrowserWindow, ipcMain } = require('electron');
const path = require('path');
const fs = require('fs');
const { pathToFileURL } = require('url');
const { GIFEncoder, quantize, applyPalette } = require('gifenc');

const GHOST_OFFSET = 20000;
const GHOST = process.env.SHOTIK_GHOST === '1';

// exportGif(videoFile, { fps, maxWidth }) -> Promise<gifPath>
function exportGif(videoFile, opts = {}) {
  return new Promise((resolve, reject) => {
    const fps = opts.fps || 12;
    const maxw = opts.maxWidth || 640;
    let meta = null;
    const frames = [];
    let settled = false;

    const win = new BrowserWindow({
      width: 200, height: 200, x: GHOST ? GHOST_OFFSET : -2000, y: 100, show: false,
      frame: false, skipTaskbar: true,
      webPreferences: {
        preload: path.join(__dirname, '..', 'gifexport', 'preload.js'),
        contextIsolation: true, nodeIntegration: false, backgroundThrottling: false,
      },
    });
    win.removeMenu();

    const cleanup = () => { try { if (!win.isDestroyed()) win.destroy(); } catch (_) {} ipcMain.removeListener('gif:meta', onMeta); ipcMain.removeListener('gif:frame', onFrame); ipcMain.removeListener('gif:done', onDone); ipcMain.removeListener('gif:error', onError); };
    const fail = (msg) => { if (settled) return; settled = true; cleanup(); reject(new Error(msg)); };
    const from = (e) => e.sender === win.webContents;

    const onMeta = (e, m) => { if (from(e)) meta = m; };
    const onFrame = (e, buf) => { if (from(e)) frames.push(Buffer.from(buf)); };
    const onError = (e, msg) => { if (from(e)) fail(msg); };
    const onDone = (e) => {
      if (!from(e) || settled) return;
      settled = true;
      try {
        if (!meta || !frames.length) throw new Error('no frames decoded');
        const gif = GIFEncoder();
        // global palette from a sample of frames (avoids per-frame flicker)
        const sample = [];
        const stride = Math.max(1, Math.floor(frames.length / 12));
        for (let i = 0; i < frames.length; i += stride) sample.push(frames[i]);
        const palette = quantize(Buffer.concat(sample), 256);
        for (const f of frames) {
          gif.writeFrame(applyPalette(f, palette), meta.w, meta.h, { palette, delay: meta.delay || 80 });
        }
        gif.finish();
        const out = videoFile.replace(/\.[^.]+$/, '') + '.gif';
        fs.writeFileSync(out, Buffer.from(gif.bytes()));
        cleanup();
        resolve(out);
      } catch (err) { cleanup(); reject(err); }
    };

    ipcMain.on('gif:meta', onMeta);
    ipcMain.on('gif:frame', onFrame);
    ipcMain.on('gif:done', onDone);
    ipcMain.on('gif:error', onError);

    win.loadFile(path.join(__dirname, '..', 'gifexport', 'gif.html'), {
      query: { src: pathToFileURL(videoFile).href, fps: String(fps), maxw: String(maxw) },
    });
    setTimeout(() => fail('gif export timeout'), 60000);
  });
}

module.exports = { exportGif };
