'use strict';
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('rec', {
  ready: (info) => ipcRenderer.send('rec:ready', info),
  chunk: (buf) => ipcRenderer.send('rec:chunk', buf),
  done: (info) => ipcRenderer.send('rec:done', info),
  firstFrame: (buf) => ipcRenderer.send('rec:first-frame', buf),
  error: (msg) => ipcRenderer.send('rec:error', msg),
  trace: (msg) => ipcRenderer.send('rec:trace', msg),
  onControl: (cb) => ipcRenderer.on('rec:control', (_e, cmd) => cb(cmd)),
});
