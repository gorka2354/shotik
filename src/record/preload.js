'use strict';
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('ctl', {
  stop: () => ipcRenderer.send('rec:controls-stop'),
  pause: () => ipcRenderer.send('rec:controls-pause'),
  cancel: () => ipcRenderer.send('rec:controls-cancel'),
  onState: (cb) => ipcRenderer.on('rec:state', (_e, s) => cb(s)),
  onLevel: (cb) => ipcRenderer.on('rec:level', (_e, l) => cb(l)),
});
