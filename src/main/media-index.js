'use strict';
// Makes MediaRecorder output seekable in ordinary players (Windows Media
// Player, VLC, browsers) without re-encoding or re-reading the file.
//
// createMediaIndexer() sits between the recorder's chunks and the file and
// picks webm-index or mp4-index by the stream's first bytes. Contract of both:
//   push(chunk) → bytes to write (may differ from chunk only in the header)
//   finish()    → { flush, append, patches, size, duration, cues, error }
// Write flush then append to the end of the file, close it, then writeIndex().

const fs = require('fs');
const { createWebmIndexer } = require('./webm-index');
const { createMp4Indexer } = require('./mp4-index');

const EMPTY = Buffer.alloc(0);

function createMediaIndexer() {
  let impl = null;
  let head = EMPTY;
  function pick() {
    impl = head.toString('latin1', 4, 8) === 'ftyp' ? createMp4Indexer() : createWebmIndexer(); // webm also rejects non-EBML cleanly
    const first = head; head = EMPTY;
    return impl.push(first);
  }
  return {
    push(chunk) {
      if (impl) return impl.push(chunk);
      if (!chunk || !chunk.length) return EMPTY;
      head = head.length ? Buffer.concat([head, chunk]) : Buffer.from(chunk);
      return head.length >= 8 ? pick() : EMPTY;
    },
    finish() {
      // a stream shorter than the sniff window is still handed over, and
      // whatever the indexer passes back must reach the file
      const early = !impl && head.length ? pick() : EMPTY;
      if (!impl) return { flush: EMPTY, append: null, patches: [], size: 0, duration: 0, cues: 0, error: 'empty stream' };
      const fin = impl.finish();
      return early.length ? { ...fin, flush: Buffer.concat([early, fin.flush]) } : fin;
    },
  };
}

// Apply finish() patches to a file that already holds flush + append.
// Returns null on success or the reason the index was not written.
function writeIndex(file, fin) {
  if (!fin.append) return fin.error || 'nothing to index';
  const actual = fs.statSync(file).size;
  if (actual !== fin.size) return `size mismatch: file ${actual}, expected ${fin.size}`;
  const fd = fs.openSync(file, 'r+');
  try { for (const p of fin.patches) fs.writeSync(fd, p.bytes, 0, p.bytes.length, p.offset); }
  finally { fs.closeSync(fd); }
  return fin.error ? 'indexed with warning: ' + fin.error : null;
}

// Index an existing (old, unseekable) recording into dst. src is untouched.
function repairFile(src, dst) {
  const idx = createMediaIndexer();
  const inFd = fs.openSync(src, 'r');
  let outFd = null;
  const buf = Buffer.alloc(4 << 20);
  try {
    outFd = fs.openSync(dst, 'w');
    for (let n; (n = fs.readSync(inFd, buf, 0, buf.length, null)) > 0;) {
      const o = idx.push(Buffer.from(buf.subarray(0, n)));
      if (o.length) fs.writeSync(outFd, o);
    }
    const fin = idx.finish();
    if (fin.flush.length) fs.writeSync(outFd, fin.flush);
    if (fin.append && fin.append.length) fs.writeSync(outFd, fin.append);
    fs.closeSync(outFd); outFd = null;
    return { ...fin, problem: writeIndex(dst, fin) };
  } finally {
    fs.closeSync(inFd);
    if (outFd !== null) fs.closeSync(outFd);
  }
}

module.exports = { createMediaIndexer, writeIndex, repairFile };
