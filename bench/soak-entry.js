'use strict';
// Electron entry point for soak runs. Test mode always uses appData/Shotik-Test with one
// single-instance lock, and Electron takes appData from the OS, not from the environment. Pointing
// it elsewhere lets several instances run side by side, and next to the ghost tests, with src/ intact.
const { app } = require('electron');

const getPath = app.getPath.bind(app);
app.getPath = (name) => (name === 'appData' && process.env.SHOTIK_APPDATA ? process.env.SHOTIK_APPDATA : getPath(name));

require('../src/main/main.js');
