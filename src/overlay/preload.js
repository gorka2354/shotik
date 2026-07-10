'use strict';
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('shotik', {
  action: (payload) => ipcRenderer.invoke('overlay:action', payload),
  cancel: () => ipcRenderer.send('overlay:cancel'),
  copyColor: (hex) => ipcRenderer.send('overlay:color', hex),
  getTheme: () => ipcRenderer.invoke('theme:get'),
  getI18n: () => ipcRenderer.invoke('i18n:get'),
  onWindows: (cb) => ipcRenderer.on('overlay:windows', (_e, list) => cb(list)),
  ocrBoxes: (png) => ipcRenderer.invoke('overlay:ocr-boxes', png),
  translate: (text) => ipcRenderer.invoke('overlay:translate', text),
});
