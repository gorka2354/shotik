'use strict';
// The only UI implementation knowledge in the contract tests lives here.
// Rust consumes semantic state/actions; Electron maps them to its reference UI.
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');
const { makeApp: launch, ELECTRON } = require('./harness');
const { probeDuration, mediaFoundation } = require('./media');

function createAdapter(app) {
  const electron = app.impl === 'electron';
  const rawPost = app.post;
  const js = (target, code, index) => rawPost('exec-js', { target, index, code });
  const capturePage = (target, file, index) => rawPost('capture-page', { target, index, path: file });

  async function recordingDiagnostics() {
    if (!electron) return (await rawPost('ui-state', { target: 'rec-controls' })).diagnostics;
    const { trace } = await rawPost('record-status');
    const source = (trace || []).join('\n').match(/vtracks=(\d+) atracks=(\d+)/);
    return {
      audioCodec: trace.some((s) => /recorder\.start audio\/mp4;codecs=mp4a\.40\.2/.test(s)) ? 'aac' : null,
      indexed: trace.some((s) => /^index ok/.test(s)),
      screenVideoTracks: source ? Number(source[1]) : null,
      screenAudioTracks: source ? Number(source[2]) : null,
    };
  }

  async function uiState(target, index) {
    if (!electron) return rawPost('ui-state', { target, index });
    switch (target) {
      case 'main': return js(target, `({
        ready: typeof state !== 'undefined' && !!state && !!DICT.recMicrophone,
        page: document.querySelector('.nav-item.active')?.dataset.page || null,
        title: document.title,
        text: {
          input: document.querySelector('#txtInput').value,
          result: document.querySelector('#txtResult').textContent,
          resultVisible: !document.querySelector('#txtResultWrap').hidden,
          loading: document.querySelector('#txtResult').classList.contains('loading')
        },
        microphone: {
          enabled: document.querySelector('#setMicrophone').checked,
          savedEnabled: typeof state !== 'undefined' && !!state && !!state.settings.recording.microphone,
          deviceRowVisible: !document.querySelector('#microphoneDeviceRow').hidden,
          deviceId: document.querySelector('#setMicrophoneDevice').value,
          deviceLabel: document.querySelector('#setMicrophoneDevice').selectedOptions[0]?.textContent || ''
        }
      })`);
      case 'tp': return js(target, `({
        original: document.getElementById('orig').textContent,
        translated: document.getElementById('trans').textContent,
        loading: document.getElementById('trans').classList.contains('loading')
      })`);
      case 'overlay': return js(target, `({
        ready: loaded, mode, selection: sel, viewport: {width: innerWidth, height: innerHeight},
        image: {width: imgW, height: imgH}, toolbarVisible: !toolbar.hidden,
        hintsVisible: !hintEl.hidden, textMode, wordCount: words.length,
        annotationCount: annotations.length
      })`, index);
      case 'rec-controls': {
        const meters = await js(target, `(() => {
          // Off-screen CSS transitions freeze. Measure actual painted geometry
          // with transitions disabled, as in the original meter regression test.
          if (!document.getElementById('ghost-meter-style')) {
            const style = document.createElement('style'); style.id = 'ghost-meter-style';
            style.textContent = '.fill{transition:none!important}'; document.head.appendChild(style);
          }
          return [...document.querySelectorAll('.meter')].map(el => ({
            input: el.dataset.input, track: el.querySelector('.track').getBoundingClientRect().width,
            fill: el.querySelector('.fill').getBoundingClientRect().width
          }));
        })()`);
        return { meters, diagnostics: await recordingDiagnostics() };
      }
      case 'pin': return js(target, `({ready: document.getElementById('img').complete && document.getElementById('img').naturalWidth > 0})`, index);
      default: throw new Error('unsupported ui-state target: ' + target);
    }
  }

  async function uiAction(target, action, index) {
    if (!electron) return rawPost('ui-action', { target, index, action });
    const a = JSON.stringify(action);
    if (target === 'main') {
      switch (action.type) {
        case 'navigate':
          await js(target, `document.querySelector('.nav-item[data-page=' + ${JSON.stringify(action.page)} + ']').click(); true`); break;
        case 'quick-translate':
          await js(target, `document.querySelector('[data-page="text"]').click(); document.querySelector('#txtInput').value = ${JSON.stringify(action.text)}; document.querySelector('#txtRun').click(); true`); break;
        case 'set-title': await js(target, `document.title = ${JSON.stringify(action.title)}; true`); break;
        case 'reload':
          await js(target, 'location.reload(); true');
          for (let i = 0; i < 50; i++) {
            await app.sleep(50);
            if ((await uiState(target)).ready) return uiState(target);
          }
          throw new Error('main UI did not become ready after reload');
        case 'microphone-toggle': await js(target, `document.querySelector('#setMicrophone').click(); true`); break;
        case 'microphone-select': await js(target, `applySettings({recording:{microphoneDeviceId:${JSON.stringify(action.deviceId)}}})`); break;
        case 'hotkey-focus':
        case 'hotkey-blur': {
          if (action.field !== 'region') throw new Error('unsupported hotkey field: ' + action.field);
          // A ghost view must not activate the native window. Send the local
          // focus transition to the real editor handler, without OS focus.
          await js(target, `(() => {
            document.querySelector('[data-page="settings"]').click();
            const el = document.querySelector('#hkRegion');
            const focusing = ${action.type === 'hotkey-focus'};
            if (focusing) {
              const rect = el.getBoundingClientRect();
              if (el.disabled || rect.width <= 0 || rect.height <= 0) throw new Error('hotkey editor is not focusable');
              el.focus();
              if (document.activeElement !== el) throw new Error('hotkey editor did not take local focus');
            } else el.blur();
            el.dispatchEvent(new FocusEvent(focusing ? 'focus' : 'blur'));
            return true;
          })()`); break;
        }
        case 'history-clear': await js(target, `window.shotik.historyClear({deleteFiles:false}).then(() => renderHistory())`); break;
        case 'history-pin': await js(target, `window.shotik.historyPin(${JSON.stringify(action.id)})`); break;
        default: throw new Error('unsupported main action: ' + action.type);
      }
    } else if (target === 'overlay') {
      switch (action.type) {
        case 'select':
          await js(target, `(() => { const a = ${a}; const p = toImg(a.rect.x, a.rect.y), q = toImg(a.rect.x+a.rect.w, a.rect.y+a.rect.h); sel = normRect(p.x,p.y,q.x,q.y); mode = 'selected'; render(); positionToolbar(); })()`, index); break;
        case 'annotate':
          await js(target, `(() => { const a = ${a}; annotations = a.annotations.map(n => {
            const p = {...n};
            for (const k of ['x','x1','x2']) if (k in p) p[k] *= kx;
            for (const k of ['y','y1','y2']) if (k in p) p[k] *= ky;
            return p;
          }); render(); updateUndoButtons(); })()`, index); break;
        case 'live-text': await js(target, 'enterTextMode()', index); break;
        case 'text-select-all': await js(target, `document.getElementById('btnTextAll').click(); true`, index); break;
        case 'copy-color': await js(target, `window.shotik.copyColor(${JSON.stringify(action.hex)}); true`, index); break;
        default: throw new Error('unsupported overlay action: ' + action.type);
      }
    } else if (target === 'bubble' && action.type === 'pointer-down') {
      const point = JSON.parse(execFileSync(ELECTRON,
        [path.join(__dirname, 'dip-point.js'), JSON.stringify(action.point)],
        { env: app.childEnv, windowsHide: true, encoding: 'utf8', timeout: 15000 }).trim());
      return rawPost('sim-down', point);
    } else throw new Error('unsupported ui-action target: ' + target);
    return uiState(target, index);
  }

  async function showMain() {
    if (!electron) return rawPost('show-main');
    // The old show-main route focuses an existing window even in ghost mode.
    // capture-page shows it with showInactive(), so the desktop keeps its focus.
    const file = path.join(app.temp, 'show-main.png');
    try { await capturePage('main', file); }
    finally { fs.rmSync(file, { force: true }); }
  }

  async function probeMedia(file, seekTo) {
    if (electron) return rawPost('media-probe', { file, seekTo });
    const duration = probeDuration(file);
    const target = seekTo || duration / 2;
    const mf = mediaFoundation(file, target);
    return { duration, seekTo: target, position: mf.positionAfterSeek,
      seeked: mf.canSeek === true && Math.abs(mf.positionAfterSeek - target) <= 0.2 };
  }

  async function waitReady(timeoutMs = 30000) {
    await app.waitReady(timeoutMs);
    // A private cold profile can expose HTTP while the first renderer/GPU is
    // still booting. Starting short UI timers or recording then races startup.
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
    post: (route, body) => route === 'show-main' ? showMain() : rawPost(route, body),
    waitReady, uiState, uiAction, capturePage, showMain, probeMedia, recordingDiagnostics,
    settingsSet: async (patch) => electron
      ? (await js('main', `window.shotik.setSettings(${JSON.stringify(patch)})`)).settings
      : rawPost('settings-set', { patch }),
    historyList: (limit = 100) => electron
      ? js('main', `window.shotik.historyList(${Number(limit)})`) : rawPost('history-list', { limit }),
    exportGif: (id) => electron
      ? js('main', `window.shotik.historyExportGif(${JSON.stringify(id)})`) : rawPost('export-gif', { id }),
    cancelOverlay: () => electron ? js('overlay', 'window.shotik.cancel(); true') : rawPost('overlay-cancel'),
    recordStart: (region = {}) => {
      if (electron && Object.keys(region).length) throw new Error('Electron record-start only supports the default test region');
      return rawPost('record-start', region);
    },
    pointerDownDip: (point) => uiAction('bubble', { type: 'pointer-down', point }),
    videoExtension: (format) => !electron || format === 'mp4' ? 'mp4' : 'webm',
    isCaptureFallback: (text) => electron ? /via desktopCapturer\b/.test(text) : /via (?!printwindow\b)\S+/i.test(text),
  };
}

const makeApp = (options) => createAdapter(launch(options));
module.exports = { makeApp, createAdapter };
