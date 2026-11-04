'use strict';
// Ghost-only launch policy; product code and its renderers remain unchanged.
const { app } = require('electron');
app.setPath('appData', process.env.APPDATA);
app.setPath('temp', process.env.TEMP);
app.on('browser-window-created', (_event, win) => {
  // Some reference windows (pins, recording controls) implicitly show during
  // construction. This event precedes that show: none may activate the desktop.
  win.setFocusable(false);
  // Recording controls use Electron's default throttling. Keep their off-screen
  // compositor alive so capture-page returns the real surface instead of empty.
  win.webContents.setBackgroundThrottling(false);
});
require('../../src/main/main');
