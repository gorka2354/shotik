'use strict';
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('shotik', {
  zoom: (id, dir) => ipcRenderer.send('pin:zoom', { id, dir }),
  close: (id) => ipcRenderer.send('pin:close', id),
  reset: (id) => ipcRenderer.send('pin:reset', id),
  menu: (id) => ipcRenderer.send('pin:menu', id),
  onZoomLevel: (cb) => ipcRenderer.on('pin:zoom-level', (_e, pct) => cb(pct)),
  getTheme: () => ipcRenderer.invoke('theme:get'),
});
