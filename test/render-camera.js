'use strict';
/* Virtual-camera renderer v2: damped follow-camera over the cinematic footage.
   - one GLOBAL palette for the whole clip (no per-frame palette flicker)
   - 2x supersampling (no shimmer on thin lines while panning)
   - holds keep the camera gliding with extra frames; static frames merge
     into longer delays instead of duplicating
     node test/render-camera.js <framesDir> <out.gif> */

const fs = require('fs');
const path = require('path');
const { PNG } = require('pngjs');
const { GIFEncoder, quantize, applyPalette } = require('gifenc');

const DIR = process.argv[2];
const OUT = process.argv[3] || 'docs/demo.gif';
const OUT_W = 960, OUT_H = 540;
const SS = 2; // supersampling factor
const SRC_W = 2560, SRC_H = 1440;
const FPS_DELAY = 80; // ~12.5 fps

const manifest = JSON.parse(fs.readFileSync(path.join(DIR, 'manifest.json'), 'utf8'));

/* ---------- camera ---------- */
function makeCam() { return { x: SRC_W / 2, y: SRC_H / 2, z: 1 }; }
function camStep(cam, t) {
  const before = { ...cam };
  cam.x += (t.x - cam.x) * 0.12;
  cam.y += (t.y - cam.y) * 0.12;
  cam.z += (t.z - cam.z) * 0.08;
  return Math.abs(cam.x - before.x) + Math.abs(cam.y - before.y) + Math.abs(cam.z - before.z) * 800;
}
function cropRect(cam) {
  const w = SRC_W / cam.z, h = SRC_H / cam.z;
  return {
    x: Math.min(Math.max(cam.x - w / 2, 0), SRC_W - w),
    y: Math.min(Math.max(cam.y - h / 2, 0), SRC_H - h),
    w, h,
  };
}

/* ---------- supersampled bilinear crop+resize ---------- */
function renderView(src, crop) {
  const W = OUT_W * SS, H = OUT_H * SS;
  const big = new Float32Array(W * H * 3);
  for (let oy = 0; oy < H; oy++) {
    const sy = crop.y + ((oy + 0.5) * crop.h) / H - 0.5;
    const y0 = Math.max(0, Math.floor(sy)), y1 = Math.min(SRC_H - 1, y0 + 1);
    const fy = sy - y0;
    for (let ox = 0; ox < W; ox++) {
      const sx = crop.x + ((ox + 0.5) * crop.w) / W - 0.5;
      const x0 = Math.max(0, Math.floor(sx)), x1 = Math.min(SRC_W - 1, x0 + 1);
      const fx = sx - x0;
      const i00 = (y0 * SRC_W + x0) * 4, i10 = (y0 * SRC_W + x1) * 4;
      const i01 = (y1 * SRC_W + x0) * 4, i11 = (y1 * SRC_W + x1) * 4;
      const o = (oy * W + ox) * 3;
      for (let c = 0; c < 3; c++) {
        const top = src[i00 + c] * (1 - fx) + src[i10 + c] * fx;
        const bot = src[i01 + c] * (1 - fx) + src[i11 + c] * fx;
        big[o + c] = top * (1 - fy) + bot * fy;
      }
    }
  }
  // box-average SS x SS -> output
  const out = Buffer.alloc(OUT_W * OUT_H * 4);
  const inv = 1 / (SS * SS);
  for (let oy = 0; oy < OUT_H; oy++) {
    for (let ox = 0; ox < OUT_W; ox++) {
      let r = 0, g = 0, b = 0;
      for (let dy = 0; dy < SS; dy++) {
        const row = ((oy * SS + dy) * W + ox * SS) * 3;
        for (let dx = 0; dx < SS; dx++) {
          r += big[row + dx * 3]; g += big[row + dx * 3 + 1]; b += big[row + dx * 3 + 2];
        }
      }
      const o = (oy * OUT_W + ox) * 4;
      out[o] = (r * inv) | 0; out[o + 1] = (g * inv) | 0; out[o + 2] = (b * inv) | 0; out[o + 3] = 255;
    }
  }
  return out;
}

/* ---------- closing shot source (desktop + toast in 2560x1440 space) ---------- */
function buildClosingSource() {
  const bg = PNG.sync.read(fs.readFileSync(path.join(__dirname, 'fake-screen.png')));
  const full = Buffer.alloc(SRC_W * SRC_H * 4);
  for (let y = 0; y < SRC_H; y++) {
    const sy = Math.min(bg.height - 1, Math.floor((y * bg.height) / SRC_H));
    for (let x = 0; x < SRC_W; x++) {
      const sx = Math.min(bg.width - 1, Math.floor((x * bg.width) / SRC_W));
      const si = (sy * bg.width + sx) * 4, di = (y * SRC_W + x) * 4;
      full[di] = bg.data[si]; full[di + 1] = bg.data[si + 1]; full[di + 2] = bg.data[si + 2]; full[di + 3] = 255;
    }
  }
  const toastPath = path.join(DIR, 'toast.png');
  if (fs.existsSync(toastPath)) {
    const t = PNG.sync.read(fs.readFileSync(toastPath));
    const ox = SRC_W - t.width - 24, oy = SRC_H - t.height - 24;
    for (let y = 0; y < t.height; y++) {
      for (let x = 0; x < t.width; x++) {
        const si = (y * t.width + x) * 4;
        const a = t.data[si + 3] / 255;
        if (a < 0.02) continue;
        const di = ((oy + y) * SRC_W + (ox + x)) * 4;
        for (let c = 0; c < 3; c++) full[di + c] = (t.data[si + c] * a + full[di + c] * (1 - a)) | 0;
      }
    }
  }
  return full;
}

/* ---------- the full camera timeline (shared by both passes) ----------
   yields { src, delayBase, holdExtra } per OUTPUT frame */
function* timeline(loadPng) {
  const cam = makeCam();
  for (const m of manifest) {
    const src = loadPng(m.file);
    const target = { x: m.x, y: m.y, z: m.zoom }; // manifest stores zoom, camera wants z
    camStep(cam, target);
    yield { src, crop: cropRect(cam) };
    // during a hold the camera keeps settling with extra frames
    const extra = Math.round((m.hold || 0) / FPS_DELAY);
    for (let i = 0; i < extra; i++) {
      const moved = camStep(cam, target);
      yield { src, crop: cropRect(cam), settled: moved < 0.35 };
    }
  }
  const closing = buildClosingSource();
  const wide = { x: SRC_W / 2, y: SRC_H / 2, z: 1 };
  for (let i = 0; i < 16; i++) {
    const moved = camStep(cam, wide);
    yield { src: closing, crop: cropRect(cam), settled: moved < 0.35, last: i === 15 };
  }
}

/* ---------- pass 1: global palette from sampled frames ---------- */
console.log('pass 1: building global palette…');
const pngCache = new Map();
const loadPng = (f) => {
  if (!pngCache.has(f)) {
    pngCache.set(f, PNG.sync.read(fs.readFileSync(path.join(DIR, f))).data);
    if (pngCache.size > 4) pngCache.delete(pngCache.keys().next().value);
  }
  return pngCache.get(f);
};
const samples = [];
{
  let i = 0;
  for (const fr of timeline(loadPng)) {
    if (i % 24 === 0) samples.push(renderView(fr.src, fr.crop));
    i++;
  }
}
const mosaic = Buffer.concat(samples);
const PALETTE = quantize(mosaic, 256);
console.log('palette ready from', samples.length, 'samples; size', PALETTE.length, 'first colors', JSON.stringify(PALETTE.slice(0, 3)));

/* ---------- pass 2: render + encode (static frames merge into delays) ---------- */
console.log('pass 2: rendering…');
const gif = GIFEncoder();
let pending = null; // { index, delay }
let written = 0, merged = 0, n = 0;
for (const fr of timeline(loadPng)) {
  n++;
  if (fr.settled && pending) {
    pending.delay += FPS_DELAY; // camera is still — extend time instead of a duplicate frame
    merged++;
  } else {
    if (pending) { gif.writeFrame(pending.index, OUT_W, OUT_H, { palette: PALETTE, delay: pending.delay }); written++; }
    const view = renderView(fr.src, fr.crop);
    pending = { index: applyPalette(view, PALETTE), delay: FPS_DELAY };
  }
  if (fr.last) pending.delay += 2600;
  if (n % 40 === 0) console.log('…', n);
}
if (pending) { gif.writeFrame(pending.index, OUT_W, OUT_H, { palette: PALETTE, delay: pending.delay }); written++; }
gif.finish();
fs.writeFileSync(OUT, Buffer.from(gif.bytes()));
console.log(`wrote ${OUT}: ${Math.round(fs.statSync(OUT).size / 1024)} KB, ${written} frames (${merged} merged into delays)`);
