'use strict';
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('gif', {
  meta: (info) => ipcRenderer.send('gif:meta', info),
  frame: (buf) => ipcRenderer.send('gif:frame', buf),
  done: () => ipcRenderer.send('gif:done'),
  error: (msg) => ipcRenderer.send('gif:error', msg),
});
