'use strict';
const q = new URLSearchParams(location.search);
const id = q.get('id');
document.getElementById('img').src = q.get('img');

window.addEventListener('wheel', (e) => window.shotik.zoom(id, e.deltaY < 0 ? 1 : -1), { passive: true });
window.addEventListener('dblclick', () => window.shotik.close(id));
window.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') window.shotik.close(id);
  if (e.key === '0') window.shotik.reset(id);
});
window.addEventListener('contextmenu', (e) => { e.preventDefault(); window.shotik.menu(id); });
document.getElementById('btnClose').addEventListener('click', () => window.shotik.close(id));
window.shotik.onZoomLevel((pct) => {
  document.getElementById('zoomLabel').textContent = pct + '%';
});
