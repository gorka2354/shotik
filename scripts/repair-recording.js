'use strict';
// Makes recordings from Shotik ≤ 1.1.2 seekable: writes "<name>-seekable.<ext>"
// next to each file (Duration + Cues for WebM, mvhd duration for MP4). No
// re-encoding; the original is never modified.
//   node scripts/repair-recording.js <file> [<file> ...]
const fs = require('fs');
const path = require('path');
const { repairFile } = require('../src/main/media-index');

const files = process.argv.slice(2);
if (!files.length) {
  console.error('usage: node scripts/repair-recording.js <recording.webm|.mp4> [...]');
  process.exit(2);
}

let failed = 0;
for (const src of files) {
  const ext = path.extname(src);
  const dst = path.join(path.dirname(src), path.basename(src, ext) + '-seekable' + ext);
  const tmp = dst + '.part';
  try {
    if (fs.existsSync(dst)) throw new Error('already exists: ' + dst);
    const r = repairFile(src, tmp);
    if (r.problem) throw new Error(r.problem);
    fs.renameSync(tmp, dst);
    console.log(`ok  ${dst}  (${(r.duration / 1000).toFixed(1)} s${r.cues ? ', ' + r.cues + ' cues' : ''})`);
  } catch (e) {
    try { fs.unlinkSync(tmp); } catch (_) {}
    console.error(`err ${src}: ${e.message}`);
    failed++;
  }
}
process.exit(failed ? 1 : 0);
