'use strict';
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('tp', {
  onData: (cb) => ipcRenderer.on('tp:data', (_e, data) => cb(data)),
  copy: (text) => ipcRenderer.send('tp:copy', text),
  close: () => ipcRenderer.send('tp:close'),
  resize: (h) => ipcRenderer.send('tp:resize', h),
  hover: (over) => ipcRenderer.send('tp:hover', over),
  getI18n: () => ipcRenderer.invoke('i18n:get'),
  getTheme: () => ipcRenderer.invoke('theme:get'),
});
