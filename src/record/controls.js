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

// Level meters for the recorded inputs (?meters=system,microphone): peak → dBFS,
// shown from -54 dB (empty) to 0 dB (full). A flat bar means nothing is coming in.
const METER_ICONS = {
  system: '<path d="M4 9v6h4l5 4V5L8 9H4z"/><path d="M16.5 8.5a5 5 0 010 7"/>',
  microphone: '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0014 0M12 18v3"/>',
};
const meterEls = {};
for (const name of (new URLSearchParams(location.search).get('meters') || '').split(',').filter((n) => METER_ICONS[n])) {
  const el = document.createElement('span');
  el.className = 'meter';
  el.dataset.input = name;
  el.innerHTML = `<svg viewBox="0 0 24 24">${METER_ICONS[name]}</svg><span class="track"><span class="fill"></span></span>`;
  document.getElementById('meters').appendChild(el);
  meterEls[name] = el;
}
window.ctl.onLevel((levels) => {
  for (const [name, el] of Object.entries(meterEls)) {
    const peak = Number(levels && levels[name]) || 0;
    const norm = peak > 0 ? Math.max(0, Math.min(1, (20 * Math.log10(peak) + 54) / 54)) : 0;
    el.querySelector('.fill').style.width = Math.round(norm * 100) + '%';
    el.classList.toggle('clip', peak >= 0.98);
  }
});
