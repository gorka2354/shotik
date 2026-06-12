'use strict';
// Screenshot history: files live in the user's save dir, the index in userData.
const { app, nativeImage } = require('electron');
const fs = require('fs');
const path = require('path');
const settings = require('./settings');

const INDEX = () => path.join(app.getPath('userData'), 'history.json');
const THUMBS = () => path.join(app.getPath('userData'), 'thumbs');
const MAX_ITEMS = 500;

let items = null;

function load() {
  if (items) return items;
  try { items = JSON.parse(fs.readFileSync(INDEX(), 'utf8')); } catch (_) { items = []; }
  return items;
}

function persist() {
  try { fs.writeFileSync(INDEX(), JSON.stringify(items, null, 1), 'utf8'); } catch (e) { console.error(e); }
}

function tsName() {
  const d = new Date();
  const p = (n) => String(n).padStart(2, '0');
  const date = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  const time = `${p(d.getHours())}-${p(d.getMinutes())}-${p(d.getSeconds())}`;
  return settings.get().fileNamePattern.replace('{date}', date).replace('{time}', time);
}

// Saves a PNG buffer to the configured dir, makes a thumb, records in history.
// Returns the history entry.
function savePng(buffer, { source = 'region', dir = null } = {}) {
  load();
  const saveDir = dir || settings.get().saveDir;
  fs.mkdirSync(saveDir, { recursive: true });
  let base = tsName();
  let file = path.join(saveDir, base + '.png');
  for (let i = 2; fs.existsSync(file); i++) file = path.join(saveDir, `${base}-${i}.png`);
  fs.writeFileSync(file, buffer);

  const img = nativeImage.createFromBuffer(buffer);
  const { width, height } = img.getSize();

  // thumbnail (max 480px wide jpeg)
  let thumb = null;
  try {
    fs.mkdirSync(THUMBS(), { recursive: true });
    const tw = Math.min(480, width);
    const small = img.resize({ width: tw });
    thumb = path.join(THUMBS(), path.basename(file, '.png') + '.jpg');
    fs.writeFileSync(thumb, small.toJPEG(82));
  } catch (_) {}

  const entry = {
    id: Date.now().toString(36) + Math.random().toString(36).slice(2, 6),
    file, thumb, width, height, ts: Date.now(), source,
  };
  items.unshift(entry);
  if (items.length > MAX_ITEMS) items = items.slice(0, MAX_ITEMS);
  persist();
  return entry;
}

function list(limit = 100) {
  load();
  // drop entries whose files were deleted externally
  const alive = items.filter((it) => fs.existsSync(it.file));
  if (alive.length !== items.length) { items = alive; persist(); }
  return items.slice(0, limit);
}

function remove(id) {
  load();
  const it = items.find((x) => x.id === id);
  if (it) {
    try { fs.unlinkSync(it.file); } catch (_) {}
    try { if (it.thumb) fs.unlinkSync(it.thumb); } catch (_) {}
    items = items.filter((x) => x.id !== id);
    persist();
  }
}

function clearAll({ deleteFiles = false } = {}) {
  load();
  if (deleteFiles) {
    for (const it of items) {
      try { fs.unlinkSync(it.file); } catch (_) {}
      try { if (it.thumb) fs.unlinkSync(it.thumb); } catch (_) {}
    }
  }
  items = [];
  persist();
}

function last() { load(); return items[0] || null; }
function get(id) { load(); return items.find((x) => x.id === id) || null; }

module.exports = { savePng, list, remove, clearAll, last, get };
