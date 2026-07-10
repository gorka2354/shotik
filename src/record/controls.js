'use strict';
let paused = false, elapsed = 0, tick = null;

function fmt(s) {
  const m = Math.floor(s / 60), ss = Math.floor(s % 60);
  return m + ':' + String(ss).padStart(2, '0');
}
function start() {
  clearInterval(tick);
  tick = setInterval(() => { if (!paused) { elapsed++; document.getElementById('time').textContent = fmt(elapsed); } }, 1000);
}
start();

document.getElementById('stop').addEventListener('click', () => window.ctl.stop());
document.getElementById('cancel').addEventListener('click', () => window.ctl.cancel());
document.getElementById('pause').addEventListener('click', () => window.ctl.pause());

window.ctl.onState((s) => {
  paused = !!s.paused;
  document.getElementById('dot').classList.toggle('paused', paused);
  document.getElementById('pauseIco').innerHTML = paused
    ? '<path d="M7 5l12 7-12 7z" fill="#d5d9e2" stroke="none"/>'  // play triangle
    : '<path d="M7 5v14M17 5v14"/>';                              // pause bars
});
