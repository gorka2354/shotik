'use strict';
let currentFile = null;
const ICONS = { text: '⎘', color: '◉', claude: '✦', repeat: '⟳' };

window.shotik.getTheme().then((t) => document.documentElement.style.setProperty('--accent', t.accent));
window.shotik.onThemeChanged((t) => document.documentElement.style.setProperty('--accent', t.accent));

window.shotik.onData((d) => {
  currentFile = d.file || null;
  document.getElementById('titleText').textContent = d.title || '';
  document.getElementById('body').textContent = d.body || '';
  const thumb = document.getElementById('thumb');
  if (d.thumb) {
    const url = encodeURI('file:///' + String(d.thumb).replace(/\\/g, '/')).replace(/['()]/g, (c) => '%' + c.charCodeAt(0).toString(16));
    thumb.style.background = `#262a36 url("${url}") center / cover no-repeat`;
    thumb.textContent = '';
  } else if (d.kind === 'color' && d.color) {
    thumb.style.background = d.color;
    thumb.textContent = '';
  } else {
    thumb.style.background = '#262a36';
    thumb.textContent = ICONS[d.kind] || '✓';
  }
});

document.getElementById('close').addEventListener('click', (e) => {
  e.stopPropagation();
  window.shotik.close();
});
document.getElementById('toast').addEventListener('click', () => {
  if (currentFile) window.shotik.openFile(currentFile);
  else window.shotik.close();
});
