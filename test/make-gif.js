'use strict';
/* Assembles README demo GIF from PNG frames: node test/make-gif.js <framesDir> <out.gif> */
const fs = require('fs');
const path = require('path');
const { PNG } = require('pngjs');
const { GIFEncoder, quantize, applyPalette } = require('gifenc');

const DIR = process.argv[2];
const OUT = process.argv[3] || 'docs/demo.gif';

// per-frame display time, ms (matched to the filming storyboard)
const DELAYS = [1500, 1500, 1600, 700, 700, 1300, 1300, 1300, 1300, 2000, 2400];

const files = fs.readdirSync(DIR).filter((f) => /^f\d+\.png$/.test(f)).sort();
if (!files.length) throw new Error('no frames in ' + DIR);

const gif = GIFEncoder();
files.forEach((f, i) => {
  const png = PNG.sync.read(fs.readFileSync(path.join(DIR, f)));
  const palette = quantize(png.data, 256);
  const index = applyPalette(png.data, palette);
  gif.writeFrame(index, png.width, png.height, {
    palette, delay: DELAYS[i] || 1200,
  });
  console.log(f, '->', DELAYS[i] || 1200, 'ms');
});
gif.finish();
fs.writeFileSync(OUT, Buffer.from(gif.bytes()));
console.log('wrote', OUT, Math.round(fs.statSync(OUT).size / 1024), 'KB');
