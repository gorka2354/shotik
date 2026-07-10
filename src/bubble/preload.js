'use strict';
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('bubble', {
  activate: () => ipcRenderer.send('bubble:activate'),
  keepalive: () => ipcRenderer.send('bubble:keepalive'),
});
