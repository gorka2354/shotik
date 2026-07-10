'use strict';
const el = (id) => document.getElementById(id);

window.tp.getTheme().then((t) => document.documentElement.style.setProperty('--accent', t.accent));
window.tp.getI18n().then(({ dict }) => {
  el('title').textContent = dict.txtTranslation || 'Translation';
  el('copy').textContent = dict.txtCopyTranslation || 'Copy translation';
});

let currentTranslation = '';

window.tp.onData((d) => {
  el('orig').textContent = d.original || '';
  const trans = el('trans');
  if (d.loading) {
    trans.classList.add('loading');
    trans.textContent = d.loadingText || 'Translating…';
    el('meta').textContent = '';
  } else if (d.error) {
    trans.classList.remove('loading');
    trans.textContent = d.error;
    el('meta').textContent = '';
  } else {
    trans.classList.remove('loading');
    trans.textContent = d.translated || '—';
    currentTranslation = d.translated || '';
    el('meta').textContent = d.source && d.source !== 'auto' ? `${d.source} → ${d.target || ''}` : '';
  }
  // report content height so main can size the window
  requestAnimationFrame(() => window.tp.resize(document.getElementById('card').offsetHeight + 16));
});

el('close').addEventListener('click', () => window.tp.close());
el('copy').addEventListener('click', () => window.tp.copy(currentTranslation));
window.addEventListener('keydown', (e) => { if (e.key === 'Escape') window.tp.close(); });
