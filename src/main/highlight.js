'use strict';
// Find the selection-highlight rectangle inside a captured region. Custom-drawn
// apps (Telegram, Steam, canvases) paint a coloured background behind the
// selected text; we sample that colour where the user dragged/clicked and return
// the bounding box of matching pixels, so OCR reads exactly the selection instead
// of a whole band. Pure function over a BGRA buffer — unit-tested, no Electron.

// bgra: Buffer, 4 bytes/px (B,G,R,A), row-major. (sx,sy): a point known to be on
// the highlight (the drag path / click). Returns {x,y,width,height} or null when
// there's no clear highlight (fall back to the plain band OCR).
function findHighlightRect(bgra, width, height, sx, sy, opts = {}) {
  const tol = opts.tol != null ? opts.tol : 40;       // per-pixel colour distance
  const minFrac = opts.minFrac != null ? opts.minFrac : 0.004; // ≥0.4% of the region
  const maxFrac = opts.maxFrac != null ? opts.maxFrac : 0.80;  // ≤80% (else it's the page bg)
  const total = width * height;
  if (total <= 0 || bgra.length < total * 4) return null;

  const px = (x, y) => { const i = (y * width + x) * 4; return [bgra[i + 2], bgra[i + 1], bgra[i]]; }; // R,G,B
  const colorful = (r, g, b) => Math.max(r, g, b) - Math.min(r, g, b); // 0 for gray (text/bg)
  const minColor = opts.minColor != null ? opts.minColor : 22;

  // Highlight colour = the most common COLOURED colour in a patch around the
  // sample point. Selection highlights are a saturated accent; text (near-black)
  // and page background (near-white/gray) are grayscale, so ignoring gray pixels
  // avoids latching onto a glyph the sample happened to land on.
  const counts = new Map();
  const R = 9;
  for (let dy = -R; dy <= R; dy++) {
    for (let dx = -R; dx <= R; dx++) {
      const x = sx + dx, y = sy + dy;
      if (x < 0 || y < 0 || x >= width || y >= height) continue;
      const [r, g, b] = px(x, y);
      if (colorful(r, g, b) < minColor) continue; // skip gray text/background
      const key = (r >> 3) + ',' + (g >> 3) + ',' + (b >> 3); // quantise to 32 levels
      const e = counts.get(key) || { n: 0, r: 0, g: 0, b: 0 };
      e.n++; e.r += r; e.g += g; e.b += b; counts.set(key, e);
    }
  }
  let best = null;
  for (const e of counts.values()) if (!best || e.n > best.n) best = e;
  if (!best) return null; // no coloured highlight near the sample → fall back
  const hr = best.r / best.n, hg = best.g / best.n, hb = best.b / best.n;
  const match = (x, y) => { const [r, g, b] = px(x, y); return Math.abs(r - hr) + Math.abs(g - hg) + Math.abs(b - hb) <= tol; };

  // Flood-fill the CONTIGUOUS highlight block from the sample point. Using a
  // connected region (not a global bbox of every matching pixel) makes it robust
  // to stray matches elsewhere — e.g. ClearType colour fringing around text that
  // happens to be near the highlight colour would otherwise blow up the bbox.
  let seedX = sx, seedY = sy;
  if (seedX < 0 || seedY < 0 || seedX >= width || seedY >= height || !match(seedX, seedY)) {
    let found = false; // sample landed on a glyph inside the highlight — seed nearby
    for (let rr = 1; rr <= R && !found; rr++) {
      for (let dy = -rr; dy <= rr && !found; dy++) for (let dx = -rr; dx <= rr && !found; dx++) {
        const x = sx + dx, y = sy + dy;
        if (x >= 0 && y >= 0 && x < width && y < height && match(x, y)) { seedX = x; seedY = y; found = true; }
      }
    }
    if (!found) return null;
  }
  const seen = new Uint8Array(total);
  const stack = [seedY * width + seedX];
  seen[stack[0]] = 1;
  let minX = width, minY = height, maxX = -1, maxY = -1, matched = 0;
  while (stack.length) {
    const idx = stack.pop();
    const x = idx % width, y = (idx - x) / width;
    matched++;
    if (x < minX) minX = x; if (x > maxX) maxX = x;
    if (y < minY) minY = y; if (y > maxY) maxY = y;
    const nb = [idx - 1, idx + 1, idx - width, idx + width];
    if (x === 0) nb[0] = -1; if (x === width - 1) nb[1] = -1;
    for (const n of nb) {
      if (n < 0 || n >= total || seen[n]) continue;
      const nx = n % width, ny = (n - nx) / width;
      if (match(nx, ny)) { seen[n] = 1; stack.push(n); }
    }
  }
  const frac = matched / total;
  if (maxX < 0 || frac < minFrac || frac > maxFrac) return null; // no clear, minority highlight
  return { x: minX, y: minY, width: maxX - minX + 1, height: maxY - minY + 1 };
}

module.exports = { findHighlightRect };
