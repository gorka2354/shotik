'use strict';
// Differences between the implementations live here; the scenario in run.js is shared.
// Electron exposes some actions only through JS inside a window (exec-js); Rust offers the same
// actions as /test/* routes (docs/rust/CONTRACT.md).
const { post } = require('./lib');

const electron = {
  name: 'electron',
  args: ['--hidden', '--test'],
  env: { SHOTIK_REAL_CAPTURE: '1', SHOTIK_FORCE_CONTROLS: '1' },
  setSettings: (port, patch) => post(port, 'exec-js', { target: 'main', code: `window.shotik.setSettings(${JSON.stringify(patch)}).then(() => true)` }),
  cancelOverlay: (port) => post(port, 'exec-js', { code: 'window.shotik.cancel(); true' }),
  async startVideo(port, { displayId, rectPhys, rectDip }) {
    const opened = await post(port, 'trigger', { mode: 'record' });
    if (!opened.overlayOpen) throw new Error('record overlay did not open');
    const payload = { action: 'record', displayId, rectPhys, rectCss: rectDip };
    // the action closes the overlay window, so no reply can come back from it
    await post(port, 'exec-js', { code: `window.shotik.action(${JSON.stringify(payload)}); true` });
  },
  historyList: (port) => post(port, 'exec-js', { target: 'main', code: 'window.shotik.historyList(5)' }),
  exportGif: (port, id) => post(port, 'exec-js', { target: 'main', code: `window.shotik.historyExportGif(${JSON.stringify(id)})` }, 300000),
};

const rust = {
  name: 'rust',
  args: ['--hidden', '--test'],
  env: { SHOTIK_REAL_CAPTURE: '1', SHOTIK_FORCE_CONTROLS: '1' },
  setSettings: (port, patch) => post(port, 'settings-set', { patch }),
  cancelOverlay: (port) => post(port, 'overlay-cancel', {}),
  startVideo: (port, { displayId, rectPhys, rectDip }) => post(port, 'record-start', { displayId, rectPhys, rectDip }),
  historyList: (port) => post(port, 'history-list', { limit: 5 }),
  exportGif: (port, id) => post(port, 'export-gif', { id }, 300000),
};

module.exports = { electron, rust };
