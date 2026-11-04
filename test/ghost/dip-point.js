'use strict';
// No windows: use the same native mapping as onMouseDown. A scale-factor
// multiplication is not the inverse on a mixed-DPI virtual desktop.
const { app, screen } = require('electron');
const fs = require('fs');
const path = require('path');
const profile = path.join(process.env.APPDATA, 'Shotik-Coordinates');
fs.mkdirSync(profile, { recursive: true });
app.setPath('appData', process.env.APPDATA);
app.setPath('userData', profile);
app.setPath('temp', process.env.TEMP);
app.whenReady().then(() => {
  const dip = JSON.parse(process.argv[2]);
  let physical = screen.dipToScreenPoint(dip);
  // Outside all displays Chromium can choose different nearest displays for
  // the two conversions. Check the inverse at the ghost point itself instead
  // of assuming dipToScreenPoint round-trips at x+20000.
  for (let i = 0; i < 8; i++) {
    const back = screen.screenToDipPoint(physical);
    if (Math.abs(back.x - dip.x) <= 1 && Math.abs(back.y - dip.y) <= 1) {
      process.stdout.write(JSON.stringify(physical));
      app.quit(); return;
    }
    const dx = screen.screenToDipPoint({ x: physical.x + 120, y: physical.y }).x - back.x;
    const dy = screen.screenToDipPoint({ x: physical.x, y: physical.y + 120 }).y - back.y;
    if (!dx || !dy) break;
    physical = { x: Math.round(physical.x + (dip.x - back.x) * 120 / dx),
      y: Math.round(physical.y + (dip.y - back.y) * 120 / dy) };
  }
  process.stderr.write('Cannot map the ghost DIP point to physical pixels');
  app.exit(1);
});
