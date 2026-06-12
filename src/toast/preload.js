'use strict';
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('shotik', {
  onData: (cb) => ipcRenderer.on('toast:data', (_e, data) => cb(data)),
  close: () => ipcRenderer.send('toast:close'),
  openFile: (file) => ipcRenderer.send('toast:open-file', file),
  getTheme: () => ipcRenderer.invoke('theme:get'),
  onThemeChanged: (cb) => ipcRenderer.on('theme:changed', (_e, t) => cb(t)),
});
