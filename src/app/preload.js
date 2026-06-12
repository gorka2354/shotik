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
  openExternal: invoke('app:open-external'),
  openSaveDir: invoke('app:open-save-dir'),
  onHistoryChanged: (cb) => ipcRenderer.on('history:changed', cb),
  onMcpLog: (cb) => ipcRenderer.on('mcp:log', (_e, entry) => cb(entry)),
});
