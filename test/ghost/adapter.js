'use strict';
// What the contract tests ask of the app, in their own words. Routes: docs/rust/TEST-ROUTES.md.
const { makeApp: launch } = require('./harness');
const { probeDuration, mediaFoundation } = require('./media');

function createAdapter(app) {
  const post = app.post;
  const uiState = (target, index) => post('ui-state', { target, index });
  const uiAction = (target, action, index) => post('ui-action', { target, index, action });
  const capturePage = (target, file, index) => post('capture-page', { target, index, path: file });
  const recordingDiagnostics = async () => (await post('ui-state', { target: 'rec-controls' })).diagnostics;
  const showMain = () => post('show-main');

  async function probeMedia(file, seekTo) {
    const duration = probeDuration(file);
    const target = seekTo || duration / 2;
    const mf = mediaFoundation(file, target);
    return { duration, seekTo: target, position: mf.positionAfterSeek,
      seeked: mf.canSeek === true && Math.abs(mf.positionAfterSeek - target) <= 0.2 };
  }

  async function waitReady(timeoutMs = 30000) {
    await app.waitReady(timeoutMs);
    // A private cold profile can expose HTTP while the main window is still building. Starting short UI
    // timers or a recording then races startup.
    const end = Date.now() + timeoutMs;
    let lastError;
    while (Date.now() < end) {
      try { if ((await uiState('main')).ready) return; }
      catch (e) { lastError = e; }
      await app.sleep(100);
    }
    throw new Error('main UI did not become ready' + (lastError ? ': ' + lastError.message : ''));
  }

  return {
    ...app,
    get port() { return app.port; },
    get child() { return app.child; },
    waitReady, uiState, uiAction, capturePage, showMain, probeMedia, recordingDiagnostics,
    settingsSet: (patch) => post('settings-set', { patch }),
    historyList: (limit = 100) => post('history-list', { limit }),
    exportGif: (id) => post('export-gif', { id }),
    cancelOverlay: () => post('overlay-cancel'),
    recordStart: (region = {}) => post('record-start', region),
    pointerDownDip: (point) => uiAction('bubble', { type: 'pointer-down', point }),
    videoExtension: () => 'mp4',
    isCaptureFallback: (text) => /via (?!printwindow\b)\S+/i.test(text),
  };
}

const makeApp = (options) => createAdapter(launch(options));
module.exports = { makeApp, createAdapter };
