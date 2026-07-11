'use strict';
// Unit tests for the selection-highlight detector (pure, no Electron).
const path = require('path');
const { test, assert, eq } = require('../tiny');
const { findHighlightRect } = require(path.join(__dirname, '..', '..', 'src', 'main', 'highlight.js'));

// Build a BGRA buffer: fill with `bg`, paint `hl` in the given rect, sprinkle a
// few `text` pixels inside it (thin glyphs, as on real highlighted text).
function makeRegion(w, h, bg, hl, rect, text) {
  const buf = Buffer.alloc(w * h * 4);
  const put = (x, y, c) => { const i = (y * w + x) * 4; buf[i] = c[2]; buf[i + 1] = c[1]; buf[i + 2] = c[0]; buf[i + 3] = 255; };
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) put(x, y, bg);
  if (rect) for (let y = rect.y; y < rect.y + rect.h; y++) for (let x = rect.x; x < rect.x + rect.w; x++) put(x, y, hl);
  if (rect && text) { // a few vertical glyph strokes inside the highlight
    for (let gx = rect.x + 3; gx < rect.x + rect.w - 2; gx += 5)
      for (let y = rect.y + 2; y < rect.y + rect.h - 2; y++) put(gx, y, text);
  }
  return buf;
}

const BG = [245, 245, 245], HL = [90, 150, 240], TEXT = [10, 10, 10];

test('finds the highlight block among normal background', () => {
  const w = 200, h = 40, rect = { x: 40, y: 12, w: 90, h: 18 };
  const buf = makeRegion(w, h, BG, HL, rect, TEXT);
  const r = findHighlightRect(buf, w, h, 70, 20); // sample inside the highlight
  assert(r, 'a rect is returned');
  // within a couple px of the painted block
  assert(Math.abs(r.x - rect.x) <= 2 && Math.abs(r.y - rect.y) <= 2, `x/y off: ${JSON.stringify(r)}`);
  assert(Math.abs(r.width - rect.w) <= 4 && Math.abs(r.height - rect.h) <= 4, `w/h off: ${JSON.stringify(r)}`);
});

test('returns null when nothing is highlighted (uniform background)', () => {
  const w = 200, h = 40;
  const buf = makeRegion(w, h, BG, BG, null, null);
  eq(findHighlightRect(buf, w, h, 100, 20), null);
});

test('returns null when the "highlight" fills almost everything (page bg)', () => {
  const w = 100, h = 30, rect = { x: 0, y: 0, w: 100, h: 30 };
  const buf = makeRegion(w, h, BG, HL, rect, null);
  eq(findHighlightRect(buf, w, h, 50, 15), null); // >80% → treated as background
});

test('returns null for a too-tiny highlight (noise)', () => {
  const w = 200, h = 40, rect = { x: 100, y: 20, w: 2, h: 2 };
  const buf = makeRegion(w, h, BG, HL, rect, null);
  eq(findHighlightRect(buf, w, h, 101, 21), null);
});

test('handles a multi-line highlight (tall block)', () => {
  const w = 220, h = 70, rect = { x: 15, y: 10, w: 180, h: 46 };
  const buf = makeRegion(w, h, BG, HL, rect, TEXT);
  const r = findHighlightRect(buf, w, h, 100, 33);
  assert(r && r.height >= 40, 'captures the full multi-line height: ' + JSON.stringify(r));
});

module.exports = {};
