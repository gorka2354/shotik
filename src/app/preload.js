'use strict';
const { contextBridge, ipcRenderer } = require('electron');

const invoke = (ch) => (...args) => ipcRenderer.invoke(ch, ...args);

contextBridge.exposeInMainWorld('shotik', {
  getState: invoke('app:state'),
  setSettings: invoke('settings:set'),
  chooseDir: invoke('settings:choose-dir'),
  historyList: invoke('history:list'),
  historyRemove: invoke('history:remove'),
  historyClear: invoke('history:clear'),
  historyCopy: invoke('history:copy'),
  historyCopyPath: invoke('history:copy-path'),
  historyPin: invoke('history:pin'),
  historyReveal: invoke('history:reveal'),
  historyOpen: invoke('history:open'),
  historyOcr: invoke('history:ocr'),
  captureRegion: invoke('capture:region'),
  captureFull: invoke('capture:full'),
  captureRepeat: invoke('capture:repeat'),
  captureText: invoke('capture:text'),
  captureRecord: invoke('capture:record'),
  recStop: invoke('rec:stop'),
  recStatus: invoke('rec:status'),
  historyExportGif: invoke('history:export-gif'),
  onRecState: (cb) => ipcRenderer.on('rec:state', (_e, s) => cb(s)),
  openExternal: invoke('app:open-external'),
  openSaveDir: invoke('app:open-save-dir'),
  translateText: invoke('translate:text'),
  copyText: invoke('app:copy-text'),
  onHistoryChanged: (cb) => ipcRenderer.on('history:changed', cb),
  onMcpLog: (cb) => ipcRenderer.on('mcp:log', (_e, entry) => cb(entry)),
  getTheme: invoke('theme:get'),
  getI18n: invoke('i18n:get'),
  onThemeChanged: (cb) => ipcRenderer.on('theme:changed', (_e, t) => cb(t)),
});
