'use strict';
/* Virtual-camera renderer: reads cinematic frames + manifest, applies a damped
   follow-camera (lazy pan towards the cursor, eased zoom per scene), composes
   the closing shot with the toast, and encodes the final GIF.
     node test/render-camera.js <framesDir> <out.gif> */

const fs = require('fs');
const path = require('path');
const { PNG } = require('pngjs');
const { GIFEncoder, quantize, applyPalette } = require('gifenc');

const DIR = process.argv[2];
const OUT = process.argv[3] || 'docs/demo.gif';
const OUT_W = 960, OUT_H = 540;
const SRC_W = 2560, SRC_H = 1440;
const FPS_DELAY = 80; // ~12.5 fps

const manifest = JSON.parse(fs.readFileSync(path.join(DIR, 'manifest.json'), 'utf8'));

// ---- camera ----
const cam = { x: SRC_W / 2, y: SRC_H / 2, z: 1 };
function camStep(target) {
  cam.x += (target.x - cam.x) * 0.12;
  cam.y += (target.y - cam.y) * 0.12;
  cam.z += (target.z - cam.z) * 0.08;
}
function cropRect() {
  const w = SRC_W / cam.z, h = SRC_H / cam.z;
  const x = Math.min(Math.max(cam.x - w / 2, 0), SRC_W - w);
  const y = Math.min(Math.max(cam.y - h / 2, 0), SRC_H - h);
  return { x, y, w, h };
}

// ---- bilinear crop+resize (always downsampling, bilinear is fine) ----
function renderView(src, crop) {
  const out = Buffer.alloc(OUT_W * OUT_H * 4);
  for (let oy = 0; oy < OUT_H; oy++) {
    const sy = crop.y + ((oy + 0.5) * crop.h) / OUT_H - 0.5;
    const y0 = Math.max(0, Math.floor(sy)), y1 = Math.min(SRC_H - 1, y0 + 1);
    const fy = sy - y0;
    for (let ox = 0; ox < OUT_W; ox++) {
      const sx = crop.x + ((ox + 0.5) * crop.w) / OUT_W - 0.5;
      const x0 = Math.max(0, Math.floor(sx)), x1 = Math.min(SRC_W - 1, x0 + 1);
      const fx = sx - x0;
      const i00 = (y0 * SRC_W + x0) * 4, i10 = (y0 * SRC_W + x1) * 4;
      const i01 = (y1 * SRC_W + x0) * 4, i11 = (y1 * SRC_W + x1) * 4;
      const o = (oy * OUT_W + ox) * 4;
      for (let c = 0; c < 3; c++) {
        const top = src[i00 + c] * (1 - fx) + src[i10 + c] * fx;
        const bot = src[i01 + c] * (1 - fx) + src[i11 + c] * fx;
        out[o + c] = (top * (1 - fy) + bot * fy) | 0;
      }
      out[o + 3] = 255;
    }
  }
  return out;
}

// ---- encode ----
const gif = GIFEncoder();
let encoded = 0;
function addFrame(rgba, delay) {
  const palette = quantize(rgba, 256);
  const index = applyPalette(rgba, palette);
  gif.writeFrame(index, OUT_W, OUT_H, { palette, delay });
  encoded++;
}

console.log('rendering', manifest.length, 'frames…');
for (const m of manifest) {
  const png = PNG.sync.read(fs.readFileSync(path.join(DIR, m.file)));
  camStep({ x: m.x, y: m.y, z: m.zoom });
  const view = renderView(png.data, cropRect());
  addFrame(view, FPS_DELAY + (m.hold || 0));
  if (encoded % 30 === 0) console.log('…', encoded);
}

// closing shot: desktop + toast, wide, long hold
const bgPath = path.join(__dirname, 'fake-screen.png');
const bg = PNG.sync.read(fs.readFileSync(bgPath));
// fixture is 1920x1080 — upscale to source space first (nearest is fine for bg)
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
// ease the camera back to wide over the closing shot
for (let i = 0; i < 14; i++) {
  camStep({ x: SRC_W / 2, y: SRC_H / 2, z: 1 });
  addFrame(renderView(full, cropRect()), i === 13 ? 2600 : FPS_DELAY);
}

gif.finish();
fs.writeFileSync(OUT, Buffer.from(gif.bytes()));
console.log('wrote', OUT, Math.round(fs.statSync(OUT).size / 1024), 'KB,', encoded, 'frames');
