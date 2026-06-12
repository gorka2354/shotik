'use strict';
// System theme integration: light/dark from the OS, accent color from
// Windows personalization / macOS appearance. Renderers get light/dark for
// free via prefers-color-scheme; the accent is pushed over IPC.
const { nativeTheme, systemPreferences } = require('electron');

const FALLBACK_ACCENT = '#0067C0'; // Windows default blue

function getAccent() {
  try {
    const c = systemPreferences.getAccentColor(); // 'RRGGBBAA'
    if (c && c.length >= 6) return '#' + c.slice(0, 6).toUpperCase();
  } catch (_) {}
  return FALLBACK_ACCENT;
}

function getTheme() {
  return {
    dark: nativeTheme.shouldUseDarkColors,
    accent: getAccent(),
    platform: process.platform,
  };
}

function watch(cb) {
  nativeTheme.on('updated', () => cb(getTheme()));
  try {
    if (process.platform === 'win32') {
      systemPreferences.on('accent-color-changed', () => cb(getTheme()));
    }
  } catch (_) {}
}

module.exports = { getTheme, watch };
