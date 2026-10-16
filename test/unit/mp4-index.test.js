'use strict';
const { test, assert, eq, near } = require('../tiny');
const { createMp4Indexer } = require('../../src/main/mp4-index');
const { createMediaIndexer } = require('../../src/main/media-index');

// ---- a synthetic fragmented MP4 shaped like Chromium's MediaRecorder output ----
const u32 = (n) => { const b = Buffer.alloc(4); b.writeUInt32BE(n); return b; };
const u64 = (n) => { const b = Buffer.alloc(8); b.writeBigUInt64BE(BigInt(n)); return b; };
const box = (type, ...parts) => { const payload = Buffer.concat(parts); return Buffer.concat([u32(payload.length + 8), Buffer.from(type, 'latin1'), payload]); };
const full = (type, version, flags, ...parts) => box(type, u32((version << 24) | flags), ...parts);

function mvhd(version, timescale) {
  return version === 1
    ? full('mvhd', 1, 0, u64(0), u64(0), u32(timescale), u64(0), Buffer.alloc(80))
    : full('mvhd', 0, 0, u32(0), u32(0), u32(timescale), u32(0), Buffer.alloc(80));
}
function trak(id, timescale) {
  return box('trak',
    full('tkhd', 1, 3, u64(0), u64(0), u32(id), u32(0), u64(0), Buffer.alloc(60)),
    box('mdia', full('mdhd', 1, 0, u64(0), u64(0), u32(timescale), u64(0), u32(0))));
}
// trun with per-sample durations (0x100) or relying on tfhd/trex defaults
function traf(id, base, durations, { perSample = true, tfhdDefault = 0 } = {}) {
  const tfhd = tfhdDefault ? full('tfhd', 0, 0x020008, u32(id), u32(tfhdDefault)) : full('tfhd', 0, 0x020000, u32(id));
  const runFlags = 0x001 | 0x200 | (perSample ? 0x100 : 0);
  const samples = durations.map((d) => Buffer.concat([...(perSample ? [u32(d)] : []), u32(10)]));
  return box('traf', tfhd, full('tfdt', 1, 0, u64(base)), full('trun', 0, runFlags, u32(durations.length), u32(0), ...samples));
}
function fmp4({ fragments = 3, mvhdVersion = 1, perSample = true, tfhdDefault = 0, trexDefault = 0 } = {}) {
  const parts = [box('ftyp', Buffer.from('isom'), u32(512), Buffer.from('isomiso2avc1mp41')),
    box('moov', mvhd(mvhdVersion, 1000), trak(1, 30000), trak(2, 48000),
      box('mvex', full('trex', 0, 0, u32(1), u32(1), u32(trexDefault), u32(0), u32(0)), full('trex', 0, 0, u32(2), u32(1), u32(0), u32(0), u32(0)))),
  ];
  let v = 0, a = 0;
  for (let f = 0; f < fragments; f++) {
    const vd = Array(30).fill(1001), ad = Array(48).fill(960);
    parts.push(box('moof', full('mfhd', 0, 0, u32(f + 1)), traf(1, v, vd, { perSample, tfhdDefault }), traf(2, a, ad)));
    parts.push(box('mdat', Buffer.alloc(5000, f)));
    v += 30 * (perSample ? 1001 : (tfhdDefault || trexDefault)); a += 48 * 960;
  }
  return { bytes: Buffer.concat(parts), seconds: Math.max(v / 30000, a / 48000) };
}

function run(input, sizes = [input.length], make = createMp4Indexer) {
  const idx = make();
  const out = [];
  for (let p = 0, k = 0; p < input.length; k++) { const n = sizes[k % sizes.length]; out.push(idx.push(input.subarray(p, p + n))); p += n; }
  const fin = idx.finish();
  const file = Buffer.concat([...out, fin.flush, fin.append || Buffer.alloc(0)]);
  for (const p of fin.patches) p.bytes.copy(file, p.offset);
  return { file, fin };
}

function mvhdDuration(file) {
  const at = file.indexOf('mvhd') + 4;
  return file[at] === 1 ? Number(file.readBigUInt64BE(at + 24)) : file.readUInt32BE(at + 16);
}

test('mp4-index: fragmented MP4 gets mvhd duration = longest track', () => {
  const src = fmp4();
  const { file, fin } = run(src.bytes);
  eq(fin.error, null);
  eq(fin.patches.length, 1);
  eq(mvhdDuration(file), Math.round(src.seconds * 1000));
  near(fin.duration, src.seconds * 1000, 0.001);
});

test('mp4-index: bytes pass through untouched except the 8 duration bytes', () => {
  const src = fmp4();
  const { file } = run(src.bytes, [333]);
  eq(file.length, src.bytes.length);
  let diff = 0;
  for (let i = 0; i < file.length; i++) if (file[i] !== src.bytes[i]) diff++;
  assert(diff >= 1 && diff <= 8, 'diff bytes ' + diff);
});

test('mp4-index: every chunking gives the same patch', () => {
  const src = fmp4({ fragments: 4 });
  const want = JSON.stringify(run(src.bytes).fin.patches);
  for (const sizes of [[1], [3], [8], [9, 1], [1000], [17, 4096]]) eq(JSON.stringify(run(src.bytes, sizes).fin.patches), want, 'chunks ' + sizes);
});

test('mp4-index: default sample durations from tfhd and from trex', () => {
  for (const opts of [{ perSample: false, tfhdDefault: 1001 }, { perSample: false, trexDefault: 1001 }]) {
    const src = fmp4(opts);
    eq(mvhdDuration(run(src.bytes).file), Math.round(src.seconds * 1000), JSON.stringify(opts));
  }
});

test('mp4-index: version-0 mvhd is patched as 32-bit', () => {
  const src = fmp4({ mvhdVersion: 0 });
  eq(mvhdDuration(run(src.bytes, [64]).file), Math.round(src.seconds * 1000));
});

test('mp4-index: a stream cut mid-fragment keeps the duration of complete fragments', () => {
  const src = fmp4({ fragments: 3 });
  const cut = src.bytes.subarray(0, src.bytes.lastIndexOf('moof') + 40);
  const { fin } = run(cut, [256]);
  near(fin.duration, 60 * 1001 / 30, 0.001, 'two whole fragments (video: 60 x 1001/30000 s)');
});

test('mp4-index: garbage box sizes stop indexing, never alter bytes', () => {
  const src = fmp4();
  const bad = Buffer.from(src.bytes);
  bad.writeUInt32BE(3, bad.indexOf('moof') - 4); // size smaller than a box header
  const { file, fin } = run(bad, [100]);
  eq(file.equals(bad), true);
  eq(fin.append, null);
  assert(fin.error, 'error reported');
});

test('mp4-index: a truncated mvhd is never patched into the next box', () => {
  const src = fmp4({ mvhdVersion: 0 });
  const at = src.bytes.indexOf('mvhd') - 4;
  // mvhd cut to 24 bytes (no duration field); the parent moov shrinks by the same amount
  const cut = 24, old = src.bytes.readUInt32BE(at);
  const moovAt = src.bytes.indexOf('moov') - 4;
  const bad = Buffer.concat([src.bytes.subarray(0, at), src.bytes.subarray(at, at + cut), src.bytes.subarray(at + old)]);
  bad.writeUInt32BE(cut, at);
  bad.writeUInt32BE(src.bytes.readUInt32BE(moovAt) - (old - cut), moovAt);
  for (const sizes of [[bad.length], [1]]) {
    const { file, fin } = run(bad, sizes);
    eq(file.equals(bad), true, 'bytes untouched, chunks ' + sizes);
    eq(fin.patches.length, 0);
    assert(/mvhd too short/.test(fin.error), String(fin.error));
  }
});

test('media-index: streams shorter than the sniff window reach the file unchanged', () => {
  for (const hex of ['deadbeef', '1a45dfa300', '1a45dfa380', '00']) {
    const input = Buffer.from(hex, 'hex');
    for (const sizes of [[input.length], [1]]) {
      const { file, fin } = run(input, sizes, createMediaIndexer);
      eq(file.toString('hex'), hex, 'output for ' + hex + ' chunks ' + sizes);
      eq(fin.size, input.length);
      eq(fin.append, null);
    }
  }
});

test('media-index: picks MP4 or WebM by magic, even from 1-byte chunks', () => {
  const mp4 = fmp4().bytes;
  const r = run(mp4, [1], createMediaIndexer);
  eq(r.fin.error, null);
  eq(r.fin.patches.length, 1);
  const webm = run(Buffer.from([0x1A, 0x45, 0xDF, 0xA3, 0x80]), [1], createMediaIndexer);
  eq(webm.file.equals(Buffer.from([0x1A, 0x45, 0xDF, 0xA3, 0x80])), true);
  eq(webm.fin.append, null);
  const empty = createMediaIndexer().finish();
  eq(empty.append, null);
  eq(empty.error, 'empty stream');
});
