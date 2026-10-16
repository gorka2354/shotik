'use strict';
// Makes MediaRecorder WebM seekable while it is being written.
//
// Chromium writes "live" WebM: the Segment and every Cluster have unknown size,
// Info has no Duration and there are no Cues. Players can play such a file but
// cannot show its length or seek. ffmpeg would fix it with a full remux; we
// avoid touching multi-GB files twice:
//
//   push(chunk)  rewrites the header once (adds a SeekHead and reserved Void
//                slots), passes every other byte through untouched and notes
//                where clusters and video keyframes are;
//   finish()     returns the Cues to append plus small in-place patches
//                (Segment/Cluster sizes, Duration, SeekHead -> Cues).
//
// If the recording is killed before finish(), the placeholders stay Void and
// sizes stay "unknown" — the file is exactly as playable as before.
// Pure module; media-index.js picks it by the stream's magic and does the fs side.

const ID = {
  EBML: 0x1A45DFA3, SEGMENT: 0x18538067, SEEKHEAD: 0x114D9B74, INFO: 0x1549A966,
  TRACKS: 0x1654AE6B, CLUSTER: 0x1F43B675, CUES: 0x1C53BB6B, TAGS: 0x1254C367,
  CHAPTERS: 0x1043A770, ATTACHMENTS: 0x1941A469,
  DURATION: 0x4489, TRACKENTRY: 0xAE, TRACKNUMBER: 0xD7, TRACKTYPE: 0x83,
  TIMECODE: 0xE7, SIMPLEBLOCK: 0xA3, BLOCKGROUP: 0xA0, BLOCK: 0xA1,
  REFERENCEBLOCK: 0xFB, BLOCKDURATION: 0x9B,
  SEEK: 0x4DBB, SEEKID: 0x53AB, SEEKPOSITION: 0x53AC,
  CUEPOINT: 0xBB, CUETIME: 0xB3, CUETRACKPOSITIONS: 0xB7, CUETRACK: 0xF7, CUECLUSTERPOSITION: 0xF1,
  VOID: 0xEC,
};
// IDs that can only live at Segment level: seeing one ends an unknown-size Cluster.
const TOP_LEVEL = new Set([ID.CLUSTER, ID.CUES, ID.TAGS, ID.CHAPTERS, ID.ATTACHMENTS,
  ID.SEEKHEAD, ID.INFO, ID.TRACKS, ID.EBML, ID.SEGMENT]);
const MAX_HEADER = 1 << 20; // MediaRecorder's header is a few hundred bytes
const SEEK_LEN = 21;        // Seek{SeekID(4 bytes), SeekPosition(8 bytes)} — fixed so it can be patched
const DURATION_LEN = 11;    // Duration as float64
const EMPTY = Buffer.alloc(0);

// ---- EBML primitives ----

// Element ID at p: { v, len } or null if b is too short. Throws on garbage.
function readId(b, p) {
  if (p >= b.length) return null;
  const first = b[p];
  let len = 1;
  while (len <= 4 && !(first & (0x80 >> (len - 1)))) len++;
  if (len > 4) throw new Error('bad element id at ' + p);
  if (p + len > b.length) return null;
  let v = 0;
  for (let i = 0; i < len; i++) v = v * 256 + b[p + i];
  return { v, len };
}

// Size vint at p: { v, len, unknown } or null if b is too short. Throws on garbage.
function readSize(b, p) {
  if (p >= b.length) return null;
  const first = b[p];
  let len = 1;
  while (len <= 8 && !(first & (0x80 >> (len - 1)))) len++;
  if (len > 8) throw new Error('bad size at ' + p);
  if (p + len > b.length) return null;
  let v = first & (0xFF >> len);
  let unknown = v === (0xFF >> len);
  for (let i = 1; i < len; i++) { v = v * 256 + b[p + i]; if (b[p + i] !== 0xFF) unknown = false; }
  return { v, len, unknown };
}

// Element header (id + size) at p, or null if incomplete.
function readHeader(b, p) {
  const id = readId(b, p);
  if (!id) return null;
  const size = readSize(b, p + id.len);
  if (!size) return null;
  return { id: id.v, size: size.v, unknown: size.unknown, sizeLen: size.len, data: p + id.len + size.len };
}

function readUint(b, p, len) {
  let v = 0;
  for (let i = 0; i < len; i++) v = v * 256 + b[p + i];
  return v;
}

function encodeId(id) {
  const bytes = [];
  for (let v = id; v > 0; v = Math.floor(v / 256)) bytes.unshift(v & 0xFF);
  return Buffer.from(bytes);
}

// Minimal-length size vint (the all-ones value of each length means "unknown").
function encodeSize(n) {
  let len = 1;
  while (n >= 2 ** (7 * len) - 1) len++;
  return fixedSize(n, len);
}

function fixedSize(n, len) {
  const out = Buffer.alloc(len);
  let v = n;
  for (let i = len - 1; i >= 0; i--) { out[i] = v % 256; v = Math.floor(v / 256); }
  out[0] |= 0x80 >> (len - 1);
  return out;
}

function uintBytes(n, len) {
  const bytes = [];
  for (let v = n; v > 0; v = Math.floor(v / 256)) bytes.unshift(v % 256);
  while (bytes.length < (len || 1)) bytes.unshift(0);
  return Buffer.from(bytes);
}

function element(id, payload) {
  return Buffer.concat([encodeId(id), encodeSize(payload.length), payload]);
}

function uintElement(id, n, len) { return element(id, uintBytes(n, len)); }

function voidElement(total) {
  // Void header is 2 bytes (ID + 1-byte size) for totals up to 128.
  return Buffer.concat([Buffer.from([ID.VOID]), encodeSize(total - 2), Buffer.alloc(total - 2)]);
}

function seekEntry(id, position) {
  const e = element(ID.SEEK, Buffer.concat([element(ID.SEEKID, encodeId(id)), uintElement(ID.SEEKPOSITION, position, 8)]));
  if (e.length !== SEEK_LEN) throw new Error('seek entry size');
  return e;
}

function durationElement(value) {
  const f = Buffer.alloc(8);
  f.writeDoubleBE(value);
  return element(ID.DURATION, f);
}

// Children of a known-size master element: [{ id, start, end, data, size }]
function children(b, start, end) {
  const out = [];
  let p = start;
  while (p < end) {
    const h = readHeader(b, p);
    if (!h || h.unknown || h.data + h.size > end) throw new Error('bad child at ' + p);
    out.push({ id: h.id, start: p, end: h.data + h.size, data: h.data, size: h.size });
    p = h.data + h.size;
  }
  return out;
}

// ---- header rewrite ----

// Parse EBML header + Segment children up to the first Cluster. Returns null
// until the whole header has arrived.
function parseHeader(b) {
  if (b.length < 4) return null;
  if (b.readUInt32BE(0) !== ID.EBML) throw new Error('not a WebM stream');
  const ebml = readHeader(b, 0);
  if (!ebml) return null;
  if (ebml.id !== ID.EBML || ebml.unknown) throw new Error('not a WebM stream');
  const ebmlEnd = ebml.data + ebml.size;
  const seg = readHeader(b, ebmlEnd);
  if (!seg) return null;
  if (seg.id !== ID.SEGMENT) throw new Error('no Segment after EBML header');
  const elements = [];
  let p = seg.data;
  for (;;) {
    const h = readHeader(b, p);
    if (!h) return null;
    if (h.id === ID.CLUSTER) return { ebmlEnd, elements, clusterStart: p };
    if (h.unknown) throw new Error('unknown-size element before first Cluster');
    if (h.data + h.size > b.length) return null;
    elements.push({ id: h.id, start: p, end: h.data + h.size, data: h.data, size: h.size });
    p = h.data + h.size;
  }
}

function videoTracks(b, tracks) {
  const out = [], all = [];
  for (const entry of children(b, tracks.data, tracks.end)) {
    if (entry.id !== ID.TRACKENTRY) continue;
    let number = 0, type = 0;
    for (const c of children(b, entry.data, entry.end)) {
      if (c.id === ID.TRACKNUMBER) number = readUint(b, c.data, c.size);
      if (c.id === ID.TRACKTYPE) type = readUint(b, c.data, c.size);
    }
    if (number) { all.push(number); if (type === 1) out.push(number); }
  }
  return { video: out, all };
}

// Builds the new header. Layout inside the Segment:
//   SeekHead[Info, Tracks, Void→Cues]  Info[..., Void→Duration]  Tracks  ...
function buildHeader(b, h) {
  const info = h.elements.find((e) => e.id === ID.INFO);
  const tracks = h.elements.find((e) => e.id === ID.TRACKS);
  if (!info || !tracks) throw new Error('header has no Info/Tracks');

  const infoKids = children(b, info.data, info.end).filter((c) => c.id !== ID.DURATION).map((c) => b.subarray(c.start, c.end));
  const newInfo = element(ID.INFO, Buffer.concat([...infoKids, voidElement(DURATION_LEN)]));
  const rest = h.elements.filter((e) => e.id !== ID.INFO && e.id !== ID.SEEKHEAD).map((e) => ({ id: e.id, bytes: b.subarray(e.start, e.end) }));

  const seekHeadLen = element(ID.SEEKHEAD, Buffer.alloc(3 * SEEK_LEN)).length;
  const infoPos = seekHeadLen;
  let pos = infoPos + newInfo.length, tracksPos = 0;
  for (const e of rest) { if (e.id === ID.TRACKS) tracksPos = pos; pos += e.bytes.length; }
  const seekHead = element(ID.SEEKHEAD, Buffer.concat([seekEntry(ID.INFO, infoPos), seekEntry(ID.TRACKS, tracksPos), voidElement(SEEK_LEN)]));

  const ebmlRaw = b.subarray(0, h.ebmlEnd);
  const segHead = Buffer.concat([encodeId(ID.SEGMENT), Buffer.from([0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])]);
  const segData = ebmlRaw.length + segHead.length;
  return {
    bytes: Buffer.concat([ebmlRaw, segHead, seekHead, newInfo, ...rest.map((e) => e.bytes)]),
    segData,                                               // absolute offset of Segment data
    segSizeAt: ebmlRaw.length + 4,                         // 8-byte Segment size
    cuesSeekAt: segData + seekHeadLen - SEEK_LEN,          // Void → Seek(Cues)
    durationAt: segData + infoPos + newInfo.length - DURATION_LEN, // Void → Duration
    tracks: videoTracks(b, tracks),
  };
}

// ---- streaming indexer ----

function createWebmIndexer() {
  let state = 'head';       // head → body | pass
  let head = EMPTY;         // buffered bytes until the first Cluster
  let error = null;
  let out = 0;              // bytes emitted so far = file size
  let hdr = null;

  // body scanner
  let pend = EMPTY;         // unparsed tail (a partial element header)
  let base = 0;             // absolute offset of pend[0]
  let skip = 0;             // payload bytes still to pass over
  const stack = [];         // open Cluster / BlockGroup: { id, end, ... }
  const clusters = [];      // { at, sizeAt, sizeLen, end }
  const cues = [];          // { time, track, cluster }
  const last = new Map();   // track → { time, delta, dur }
  let cueTrack = 0;

  function fail(e) { if (!error) error = String((e && e.message) || e); state = state === 'head' ? 'pass' : state; }

  function frame(track, time, key, dur, cluster) {
    const t = last.get(track) || { time: -Infinity, delta: 0, dur: 0 };
    if (time > t.time) { if (t.time !== -Infinity) t.delta = time - t.time; t.time = time; t.dur = dur || 0; }
    last.set(track, t);
    if (!cluster) return;
    if (track === cueTrack && key && !cluster.cued) { cluster.cued = true; cues.push({ time, track, cluster: cluster.at - hdr.segData }); }
  }

  function close(c) {
    if (c.id === ID.CLUSTER) {
      if (!hdr.tracks.video.length && !c.cued && c.tc != null) { c.cued = true; cues.push({ time: c.tc, track: cueTrack, cluster: c.at - hdr.segData }); }
    } else if (c.id === ID.BLOCKGROUP && c.track) {
      frame(c.track, c.time, c.key, c.dur, c.cluster);
    }
  }

  function cluster() { for (let i = stack.length - 1; i >= 0; i--) if (stack[i].id === ID.CLUSTER) return stack[i]; return null; }

  function scan(chunk) {
    const b = pend.length ? Buffer.concat([pend, chunk]) : chunk;
    let i = 0;
    for (;;) {
      if (skip) { const n = Math.min(skip, b.length - i); i += n; skip -= n; if (skip) break; }
      const abs = base + i;
      while (stack.length && abs >= stack[stack.length - 1].end) close(stack.pop());
      const h = readHeader(b, i);
      if (!h) break;
      let top = stack[stack.length - 1];
      if (top && top.end === Infinity && TOP_LEVEL.has(h.id)) {
        // an unknown-size Cluster ends where the next top-level element starts
        top.endAt = abs; close(stack.pop()); top = stack[stack.length - 1];
      }
      const dataAbs = base + h.data;
      const end = h.unknown ? Infinity : dataAbs + h.size;
      const need = (n) => h.data + Math.min(h.size, n) <= b.length;

      if (!top) {
        if (h.id === ID.CLUSTER) {
          const c = { id: ID.CLUSTER, at: abs, end, endAt: end, tc: null, cued: false, sizeAt: abs + (h.data - i) - h.sizeLen, sizeLen: h.sizeLen, unknown: h.unknown };
          clusters.push(c); stack.push(c); i = h.data;
        } else {
          if (h.unknown) throw new Error('unknown-size element at ' + abs);
          i = h.data; skip = h.size;
        }
      } else if (top.id === ID.CLUSTER) {
        if (h.id === ID.TIMECODE) {
          if (h.size > 8) throw new Error('bad Cluster timecode at ' + abs);
          if (!need(8)) break;
          top.tc = readUint(b, h.data, h.size); i = h.data + h.size;
        } else if (h.id === ID.SIMPLEBLOCK) {
          if (!need(11)) break;
          const tn = readSize(b, h.data);
          const time = (top.tc || 0) + b.readInt16BE(h.data + tn.len);
          frame(tn.v, time, !!(b[h.data + tn.len + 2] & 0x80), 0, top);
          i = h.data; skip = h.size;
        } else if (h.id === ID.BLOCKGROUP) {
          if (h.unknown) throw new Error('unknown-size BlockGroup at ' + abs);
          stack.push({ id: ID.BLOCKGROUP, end, track: 0, time: 0, key: true, dur: 0, cluster: top }); i = h.data;
        } else {
          if (h.unknown) throw new Error('unknown-size element in Cluster at ' + abs);
          i = h.data; skip = h.size;
        }
      } else { // inside BlockGroup
        if (h.id === ID.BLOCK) {
          if (!need(11)) break;
          const tn = readSize(b, h.data);
          top.track = tn.v; top.time = (top.cluster.tc || 0) + b.readInt16BE(h.data + tn.len);
          i = h.data; skip = h.size;
        } else if (h.id === ID.BLOCKDURATION) {
          if (h.size > 8) throw new Error('bad BlockDuration at ' + abs);
          if (!need(8)) break;
          top.dur = readUint(b, h.data, h.size); i = h.data + h.size;
        } else {
          if (h.id === ID.REFERENCEBLOCK) top.key = false;
          i = h.data; skip = h.size;
        }
      }
    }
    pend = i < b.length ? Buffer.from(b.subarray(i)) : EMPTY;
    base += i;
  }

  function body(chunk) {
    if (state === 'body' && chunk.length) { try { scan(chunk); } catch (e) { error = String(e.message || e); state = 'pass'; } }
    out += chunk.length;
    return chunk;
  }

  function push(chunk) {
    if (!chunk || !chunk.length) return EMPTY;
    if (state !== 'head') return body(chunk);
    head = head.length ? Buffer.concat([head, chunk]) : Buffer.from(chunk);
    let parsed;
    try {
      parsed = parseHeader(head);
      if (!parsed) {
        if (head.length > MAX_HEADER) throw new Error('header too large');
        return EMPTY;
      }
      hdr = buildHeader(head, parsed);
    } catch (e) {
      fail(e);
      const raw = head; head = EMPTY;
      out += raw.length;
      return raw;
    }
    cueTrack = hdr.tracks.video[0] || hdr.tracks.all[0] || 1;
    state = 'body';
    out = hdr.bytes.length; base = out;
    const rest = head.subarray(parsed.clusterStart);
    head = EMPTY;
    return Buffer.concat([hdr.bytes, body(Buffer.from(rest))]);
  }

  // → { flush, append, patches, size, duration, cues, error }
  // Write flush, then append, to the end of the file; then apply patches
  // (only if the file is exactly `size` bytes — otherwise offsets are wrong).
  function finish() {
    const flush = head; head = EMPTY;
    out += flush.length;
    if (state !== 'body') return { flush, append: null, patches: [], size: out, duration: 0, cues: 0, error: error || 'no clusters' };
    while (stack.length) { const c = stack.pop(); if (c.id === ID.CLUSTER && c.end === Infinity) c.endAt = out; close(c); }
    if (!cues.length) return { flush, append: null, patches: [], size: out, duration: 0, cues: 0, error: 'no keyframes' };

    let duration = 0;
    for (const t of last.values()) duration = Math.max(duration, t.time + (t.dur || t.delta));
    const cuesPos = out - hdr.segData;
    const append = element(ID.CUES, Buffer.concat(cues.map((c) => element(ID.CUEPOINT, Buffer.concat([
      uintElement(ID.CUETIME, c.time),
      element(ID.CUETRACKPOSITIONS, Buffer.concat([uintElement(ID.CUETRACK, c.track), uintElement(ID.CUECLUSTERPOSITION, c.cluster)])),
    ])))));
    const size = out + append.length;
    const patches = [
      { offset: hdr.segSizeAt, bytes: fixedSize(size - hdr.segData, 8) },
      { offset: hdr.durationAt, bytes: durationElement(duration) },
      { offset: hdr.cuesSeekAt, bytes: seekEntry(ID.CUES, cuesPos) },
    ];
    // Known Cluster sizes, where the writer left room for them (Chromium: 8-byte unknown).
    for (const c of clusters) {
      if (!c.unknown || c.endAt === Infinity) continue;
      const len = c.endAt - (c.sizeAt + c.sizeLen);
      if (len < 2 ** (7 * c.sizeLen) - 1) patches.push({ offset: c.sizeAt, bytes: fixedSize(len, c.sizeLen) });
    }
    return { flush, append, patches, size, duration, cues: cues.length, error: error || null };
  }

  return { push, finish, get error() { return error; } };
}

module.exports = { createWebmIndexer, _ebml: { readHeader, readSize, element, uintElement, encodeId, encodeSize, children, ID } };
