'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');
const { test, assert, eq, near } = require('../tiny');
const { createWebmIndexer, _ebml } = require('../../src/main/webm-index');
const { writeIndex, repairFile } = require('../../src/main/media-index');

const { element, uintElement, encodeId, readHeader, children, ID } = _ebml;
const UNKNOWN8 = Buffer.from([0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);

// ---- a synthetic "live" WebM shaped like Chromium's MediaRecorder output ----
function block(id, track, rel, flags, size = 40) {
  const head = Buffer.from([0x80 | track, (rel >> 8) & 0xFF, rel & 0xFF, flags]);
  return element(id, Buffer.concat([head, Buffer.alloc(size, track)]));
}
// video frame as Chromium writes it with alpha: BlockGroup{Block, ReferenceBlock if delta}
function videoGroup(rel, key) {
  return element(ID.BLOCKGROUP, Buffer.concat([block(ID.BLOCK, 1, rel, 0, 120), ...(key ? [] : [element(ID.REFERENCEBLOCK, Buffer.from([0xFF, 0xDF]))])]));
}
function liveWebm({ clusters = 4, perCluster = 10, frameMs = 33, videoAs = 'group', info = [] } = {}) {
  const ebml = element(ID.EBML, Buffer.concat([uintElement(0x4286, 1), element(0x4282, Buffer.from('webm'))]));
  const infoEl = element(ID.INFO, Buffer.concat([uintElement(0x2AD7B1, 1000000), element(0x4D80, Buffer.from('Chrome')), ...info]));
  const tracks = element(ID.TRACKS, Buffer.concat([
    element(ID.TRACKENTRY, Buffer.concat([uintElement(ID.TRACKNUMBER, 1), uintElement(ID.TRACKTYPE, 1), element(0x86, Buffer.from('V_VP9'))])),
    element(ID.TRACKENTRY, Buffer.concat([uintElement(ID.TRACKNUMBER, 2), uintElement(ID.TRACKTYPE, 2), element(0x86, Buffer.from('A_OPUS'))])),
  ]));
  const parts = [ebml, encodeId(ID.SEGMENT), UNKNOWN8, infoEl, tracks];
  const keyTimes = [];
  for (let c = 0; c < clusters; c++) {
    const tc = c * perCluster * frameMs;
    parts.push(encodeId(ID.CLUSTER), UNKNOWN8, uintElement(ID.TIMECODE, tc));
    for (let f = 0; f < perCluster; f++) {
      const rel = f * frameMs, key = f === 0;
      if (key) keyTimes.push(tc + rel);
      parts.push(videoAs === 'group' ? videoGroup(rel, key) : block(ID.SIMPLEBLOCK, 1, rel, key ? 0x80 : 0, 120));
      parts.push(block(ID.SIMPLEBLOCK, 2, rel + 5, 0x80));
    }
  }
  return { bytes: Buffer.concat(parts), keyTimes, lastVideo: (clusters * perCluster - 1) * frameMs };
}

// feed `input` in chunks of the given sizes (cycled), return file bytes + finish()
function run(input, sizes = [input.length]) {
  const idx = createWebmIndexer();
  const out = [];
  for (let p = 0, k = 0; p < input.length; k++) {
    const n = sizes[k % sizes.length];
    out.push(idx.push(input.subarray(p, p + n)));
    p += n;
  }
  const fin = idx.finish();
  out.push(fin.flush);
  if (fin.append) out.push(fin.append);
  const file = Buffer.concat(out);
  for (const p of fin.patches) p.bytes.copy(file, p.offset);
  return { file, fin };
}

// Strict walk of the indexed file: every size must be known and exact.
function inspect(file) {
  const ebml = readHeader(file, 0);
  const seg = readHeader(file, ebml.data + ebml.size);
  assert(seg.id === ID.SEGMENT && !seg.unknown, 'Segment has a known size');
  eq(seg.data + seg.size, file.length, 'Segment size covers the file exactly');
  const top = children(file, seg.data, file.length); // throws on any unknown/overrun size
  const at = (rel) => readHeader(file, seg.data + rel).id;
  const seekHead = top.find((e) => e.id === ID.SEEKHEAD);
  const seeks = {};
  for (const s of children(file, seekHead.data, seekHead.end).filter((e) => e.id === ID.SEEK)) {
    const [sid, spos] = children(file, s.data, s.end);
    seeks[file.readUIntBE(sid.data, sid.size)] = file.readUIntBE(spos.data + 2, 6) + file.readUInt16BE(spos.data) * 2 ** 48;
  }
  for (const [id, rel] of Object.entries(seeks)) eq(at(rel), Number(id), 'SeekHead entry points at its element');
  const info = top.find((e) => e.id === ID.INFO);
  const durations = children(file, info.data, info.end).filter((e) => e.id === ID.DURATION);
  eq(durations.length, 1, 'exactly one Duration');
  const cuesEl = top.find((e) => e.id === ID.CUES);
  const cues = children(file, cuesEl.data, cuesEl.end).map((cp) => {
    const [time, pos] = children(file, cp.data, cp.end);
    const tp = children(file, pos.data, pos.end);
    const cp2 = tp.find((e) => e.id === ID.CUECLUSTERPOSITION);
    return { time: file.readUIntBE(time.data, time.size), track: file[tp[0].data], cluster: file.readUIntBE(cp2.data, cp2.size) };
  });
  for (const c of cues) eq(at(c.cluster), ID.CLUSTER, 'cue points at a Cluster');
  return { top, seeks, duration: file.readDoubleBE(durations[0].data), cues, clusters: top.filter((e) => e.id === ID.CLUSTER).length };
}

test('webm-index: live MediaRecorder WebM gets Duration, Cues, SeekHead and known sizes', () => {
  const src = liveWebm();
  const { file, fin } = run(src.bytes);
  eq(fin.error, null);
  const r = inspect(file);
  eq(r.clusters, 4);
  eq(r.cues.map((c) => c.time), src.keyTimes, 'one cue per video keyframe');
  assert(r.cues.every((c) => c.track === 1), 'cues reference the video track');
  near(r.duration, src.lastVideo + 5 + 33, 0.001, 'duration = last packet (audio, +5ms) + its interval');
  assert(String(ID.CUES) in r.seeks, 'SeekHead knows where the Cues are');
});

test('webm-index: every chunking of the stream gives the identical file', () => {
  const src = liveWebm({ clusters: 3, perCluster: 7 });
  const whole = run(src.bytes).file;
  for (const sizes of [[1], [2], [3], [7], [11, 1, 400], [12], [13, 5, 1], [189], [190], [4096]]) {
    eq(run(src.bytes, sizes).file.equals(whole), true, 'chunks ' + sizes.join(','));
  }
});

test('webm-index: payload bytes after the header are untouched (only size fields change)', () => {
  const src = liveWebm({ clusters: 2, perCluster: 3 });
  const { file } = run(src.bytes);
  const oldBody = src.bytes.subarray(src.bytes.indexOf(encodeId(ID.CLUSTER)));
  const newBody = file.subarray(file.indexOf(encodeId(ID.CLUSTER)), file.lastIndexOf(encodeId(ID.CUES)));
  eq(newBody.length, oldBody.length);
  let diff = 0;
  for (let i = 0; i < oldBody.length; i++) if (oldBody[i] !== newBody[i]) diff++;
  assert(diff > 0 && diff <= 2 * 7, 'only the two cluster size fields differ, got ' + diff);
});

test('webm-index: SimpleBlock keyframes (no alpha) are indexed too', () => {
  const src = liveWebm({ videoAs: 'simple' });
  const r = inspect(run(src.bytes, [97]).file);
  eq(r.cues.map((c) => c.time), src.keyTimes);
});

test('webm-index: an existing Duration is replaced, not duplicated', () => {
  const f = Buffer.alloc(8); f.writeDoubleBE(1);
  const src = liveWebm({ info: [element(ID.DURATION, f)] });
  const r = inspect(run(src.bytes).file);
  near(r.duration, src.lastVideo + 5 + 33, 0.001);
});

test('webm-index: recording cut mid-block still yields a valid index for what was written', () => {
  const src = liveWebm({ clusters: 3, perCluster: 10 });
  const cut = src.bytes.subarray(0, src.bytes.length - 60);
  const { file, fin } = run(cut, [500]);
  eq(fin.cues, 3);
  // the torn last block makes the tail unparseable for strict tools; the header and cues must still be right
  const ebml = readHeader(file, 0);
  const seg = readHeader(file, ebml.data + ebml.size);
  eq(seg.data + seg.size, file.length);
});

test('webm-index: a non-WebM stream (MP4) passes through byte-for-byte', () => {
  const mp4 = Buffer.concat([Buffer.from([0, 0, 0, 0x18]), Buffer.from('ftypisom'), Buffer.alloc(300, 7)]);
  const { file, fin } = run(mp4, [5, 50]);
  eq(file.equals(mp4), true);
  eq(fin.append, null);
  assert(/not a WebM/.test(fin.error), fin.error);
});

test('webm-index: garbage inside a cluster stops indexing but never alters the bytes', () => {
  const src = liveWebm({ clusters: 2, perCluster: 4 });
  const bad = Buffer.from(src.bytes);
  const second = bad.indexOf(encodeId(ID.CLUSTER), bad.indexOf(encodeId(ID.CLUSTER)) + 1);
  bad.fill(0, second + 12, second + 20); // zero bytes are not a valid element ID
  const idx = createWebmIndexer();
  const body = idx.push(bad);
  const fin = idx.finish();
  const out = Buffer.concat([body, fin.flush]);
  eq(fin.append, null);
  assert(fin.error, 'error is reported');
  eq(out.subarray(out.indexOf(encodeId(ID.CLUSTER))).equals(bad.subarray(bad.indexOf(encodeId(ID.CLUSTER)))), true);
});

test('webm-index: stop before the first cluster flushes the header unchanged', () => {
  const src = liveWebm();
  const head = src.bytes.subarray(0, src.bytes.indexOf(encodeId(ID.CLUSTER)));
  const { file, fin } = run(head, [16]);
  eq(file.equals(head), true);
  eq(fin.append, null);
});

test('webm-index: writeIndex refuses to patch a file of the wrong size', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-webm-'));
  try {
    const src = liveWebm();
    const idx = createWebmIndexer();
    const body = idx.push(src.bytes);
    const fin = idx.finish();
    const file = path.join(dir, 'a.webm');
    fs.writeFileSync(file, Buffer.concat([body, fin.append.subarray(1)])); // one byte short
    assert(/size mismatch/.test(writeIndex(file, fin)));
    fs.writeFileSync(file, Buffer.concat([body, fin.append]));
    eq(writeIndex(file, fin), null);
    inspect(fs.readFileSync(file));
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test('webm-index: repairFile indexes an old recording into a new file, source untouched', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-webm-'));
  try {
    const src = liveWebm({ clusters: 5 });
    const a = path.join(dir, 'old.webm'), b = path.join(dir, 'fixed.webm');
    fs.writeFileSync(a, src.bytes);
    const r = repairFile(a, b);
    eq(r.problem, null);
    eq(fs.readFileSync(a).equals(src.bytes), true);
    eq(inspect(fs.readFileSync(b)).cues.length, 5);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
