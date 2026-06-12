'use strict';
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('shotik', {
  onData: (cb) => ipcRenderer.on('toast:data', (_e, data) => cb(data)),
  close: () => ipcRenderer.send('toast:close'),
  openFile: (file) => ipcRenderer.send('toast:open-file', file),
});
