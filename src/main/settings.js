'use strict';
// Tiny JSON settings store (no deps). Lives in userData/settings.json.
const { app } = require('electron');
const fs = require('fs');
const path = require('path');

const FILE = () => path.join(app.getPath('userData'), 'settings.json');

const DEFAULTS = () => ({
  saveDir: process.env.SHOTIK_SAVE_DIR || path.join(app.getPath('pictures'), 'Shotik'),
  fileNamePattern: 'Shot_{date}_{time}',
  autoSave: true,
  smartClipboard: true, // clipboard gets BOTH image and file path text
  showToast: true,
  hotkeys: {
    region: 'PrintScreen',
    fullscreen: 'Ctrl+PrintScreen',
    repeatLast: 'Shift+PrintScreen',
  },
  mcp: { enabled: true, port: 7464 },
  launchAtStartup: false,
  downscaleForAI: true, // MCP screenshots resized to <=1600px for token economy
  lastRegion: null, // remembered for the "repeat last region" hotkey
});

let cache = null;

function load() {
  if (cache) return cache;
  let data = {};
  try { data = JSON.parse(fs.readFileSync(FILE(), 'utf8')); } catch (_) {}
  cache = deepMerge(DEFAULTS(), data);
  return cache;
}

function save() {
  try {
    fs.mkdirSync(path.dirname(FILE()), { recursive: true });
    fs.writeFileSync(FILE(), JSON.stringify(cache, null, 2), 'utf8');
  } catch (e) { console.error('settings save failed', e); }
}

function get() { return load(); }

function set(patch) {
  cache = deepMerge(load(), patch);
  save();
  return cache;
}

function deepMerge(base, over) {
  const out = Array.isArray(base) ? [...base] : { ...base };
  for (const k of Object.keys(over || {})) {
    const v = over[k];
    if (v && typeof v === 'object' && !Array.isArray(v) && base[k] && typeof base[k] === 'object' && !Array.isArray(base[k])) {
      out[k] = deepMerge(base[k], v);
    } else if (v !== undefined) {
      out[k] = v;
    }
  }
  return out;
}

module.exports = { get, set, DEFAULTS };
