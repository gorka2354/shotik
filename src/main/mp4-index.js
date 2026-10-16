'use strict';
// Gives MediaRecorder's fragmented MP4 a real duration while it is written.
//
// Chromium writes ftyp + moov (mvhd duration = 0) and then moof/mdat
// fragments. Chromium itself copes, but Media Foundation (Windows Media
// Player) reports an unknown length, so its timeline cannot be scrubbed.
// We pass every byte through untouched, sum each track's fragment durations
// (tfdt + trun), and finish() returns one in-place patch: mvhd.duration.
// Pure module — same push()/finish() contract as webm-index.js.

const MAX_BOX = 16 << 20; // moov/moof are KBs; anything bigger is not ours to buffer
const EMPTY = Buffer.alloc(0);

function boxes(b, start, end) {
  const out = [];
  for (let p = start; p + 8 <= end;) {
    let size = b.readUInt32BE(p), head = 8;
    if (size === 1) { size = Number(b.readBigUInt64BE(p + 8)); head = 16; }
    if (size < head || p + size > end) throw new Error('bad box at ' + p);
    out.push({ type: b.toString('latin1', p + 4, p + 8), start: p, data: p + head, end: p + size });
    p += size;
  }
  return out;
}

function find(b, box, path) {
  let level = [box];
  for (const type of path) level = level.flatMap((x) => boxes(b, x.data, x.end)).filter((x) => x.type === type);
  return level;
}

// Index of a field inside a box's payload; throws if the box is too short, so a
// malformed box can never steer a read — or the duration patch — into its neighbour.
function field(box, offset, len) {
  if (box.data + offset + len > box.end) throw new Error(box.type + ' too short');
  return box.data + offset;
}

function createMp4Indexer() {
  let pend = EMPTY;   // bytes of the box currently being collected (moov/moof)
  let skip = 0;       // payload bytes to pass over (mdat, ...)
  let out = 0;        // bytes emitted = file size
  let error = null;
  let mvhd = null;    // { durationAt, durationLen, timescale }
  const tracks = new Map(); // track_ID → { timescale, defaultDuration, end }

  function onMoov(b, box, at) {
    const mv = find(b, box, ['mvhd'])[0];
    if (!mv) throw new Error('moov without mvhd');
    const v1 = b[field(mv, 0, 1)] === 1;
    const durationLen = v1 ? 8 : 4;
    const durationAt = field(mv, v1 ? 24 : 16, durationLen);
    mvhd = { durationAt: at + (durationAt - box.start), durationLen, timescale: b.readUInt32BE(field(mv, v1 ? 20 : 12, 4)) };
    for (const trak of find(b, box, ['trak'])) {
      const tk = find(b, trak, ['tkhd'])[0], md = find(b, trak, ['mdia', 'mdhd'])[0];
      if (!tk || !md) continue;
      const id = b.readUInt32BE(field(tk, b[field(tk, 0, 1)] === 1 ? 20 : 12, 4));
      tracks.set(id, { timescale: b.readUInt32BE(field(md, b[field(md, 0, 1)] === 1 ? 20 : 12, 4)), defaultDuration: 0, end: 0 });
    }
    for (const trex of find(b, box, ['mvex', 'trex'])) {
      const t = tracks.get(b.readUInt32BE(field(trex, 4, 4)));
      if (t) t.defaultDuration = b.readUInt32BE(field(trex, 12, 4));
    }
  }

  function onMoof(b, box) {
    for (const traf of find(b, box, ['traf'])) {
      const tf = find(b, traf, ['tfhd'])[0];
      if (!tf) continue;
      const flags = b.readUInt32BE(field(tf, 0, 4)) & 0xFFFFFF;
      const t = tracks.get(b.readUInt32BE(field(tf, 4, 4)));
      if (!t) continue;
      const defaultDuration = flags & 0x08 ? b.readUInt32BE(field(tf, 8 + (flags & 0x01 ? 8 : 0) + (flags & 0x02 ? 4 : 0), 4)) : t.defaultDuration;
      const dt = find(b, traf, ['tfdt'])[0];
      let time = t.end;
      if (dt) time = b[field(dt, 0, 1)] === 1 ? Number(b.readBigUInt64BE(field(dt, 4, 8))) : b.readUInt32BE(field(dt, 4, 4));
      for (const run of find(b, traf, ['trun'])) {
        const rf = b.readUInt32BE(field(run, 0, 4)) & 0xFFFFFF;
        const count = b.readUInt32BE(field(run, 4, 4));
        const per = [0x100, 0x200, 0x400, 0x800].filter((f) => rf & f).length * 4;
        let q = field(run, 8 + (rf & 0x01 ? 4 : 0) + (rf & 0x04 ? 4 : 0), count * per);
        for (let i = 0; i < count; i++, q += per) time += rf & 0x100 ? b.readUInt32BE(q) : defaultDuration;
      }
      t.end = Math.max(t.end, time);
    }
  }

  function scan(chunk) {
    let b = pend.length ? Buffer.concat([pend, chunk]) : chunk;
    let p = 0;
    for (;;) {
      if (skip) { const n = Math.min(skip, b.length - p); p += n; skip -= n; if (skip) break; }
      if (p + 8 > b.length) break;
      let size = b.readUInt32BE(p), head = 8;
      const type = b.toString('latin1', p + 4, p + 8);
      if (size === 1) { if (p + 16 > b.length) break; size = Number(b.readBigUInt64BE(p + 8)); head = 16; }
      if (size === 0) { skip = Infinity; p += head; continue; } // box runs to end of file
      if (size < head) throw new Error('bad box size at ' + (out - (b.length - p)));
      if (type !== 'moov' && type !== 'moof') { p += head; skip = size - head; continue; }
      if (size > MAX_BOX) throw new Error(type + ' too large');
      if (p + size > b.length) break; // wait for the whole moov/moof
      const box = { type, start: p, data: p + head, end: p + size };
      const at = out - (b.length - p);
      if (type === 'moov') onMoov(b, box, at); else if (mvhd) onMoof(b, box);
      p += size;
    }
    pend = p < b.length ? Buffer.from(b.subarray(p)) : EMPTY;
  }

  function push(chunk) {
    if (!chunk || !chunk.length) return EMPTY;
    out += chunk.length;
    if (!error) { try { scan(chunk); } catch (e) { error = String(e.message || e); pend = EMPTY; } }
    return chunk;
  }

  function finish() {
    const base = { flush: EMPTY, size: out, cues: 0 };
    if (error || !mvhd) return { ...base, append: null, patches: [], duration: 0, error: error || 'no moov' };
    let seconds = 0;
    for (const t of tracks.values()) if (t.timescale) seconds = Math.max(seconds, t.end / t.timescale);
    const duration = Math.round(seconds * mvhd.timescale);
    if (!duration) return { ...base, append: null, patches: [], duration: 0, error: 'no fragments' };
    if (duration >= 2 ** (8 * mvhd.durationLen)) return { ...base, append: null, patches: [], duration, error: 'duration overflows mvhd' };
    const bytes = Buffer.alloc(mvhd.durationLen);
    if (mvhd.durationLen === 8) bytes.writeBigUInt64BE(BigInt(duration)); else bytes.writeUInt32BE(duration);
    return { ...base, append: EMPTY, patches: [{ offset: mvhd.durationAt, bytes }], duration: seconds * 1000, error: null };
  }

  return { push, finish, get error() { return error; } };
}

module.exports = { createMp4Indexer };
