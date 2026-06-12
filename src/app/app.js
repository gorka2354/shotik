'use strict';
/* Shotik main window UI */

let state = null;

const $ = (s) => document.querySelector(s);
const $$ = (s) => [...document.querySelectorAll(s)];

/* ---------- navigation ---------- */
$$('.nav-item').forEach((b) => {
  b.addEventListener('click', () => {
    $$('.nav-item').forEach((x) => x.classList.toggle('active', x === b));
    $$('.page').forEach((p) => p.classList.toggle('active', p.id === 'page-' + b.dataset.page));
  });
});

/* ---------- system theme / accent ---------- */
function applyTheme(t) {
  const root = document.documentElement;
  root.style.setProperty('--accent', t.accent);
  const n = parseInt(t.accent.slice(1), 16);
  const lum = 0.299 * ((n >> 16) & 255) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255);
  root.style.setProperty('--accent-text', lum > 160 ? '#1b1b1b' : '#ffffff');
  root.dataset.platform = t.platform;
}
window.shotik.onThemeChanged(applyTheme);

/* ---------- boot ---------- */
async function boot() {
  applyTheme(await window.shotik.getTheme());
  state = await window.shotik.getState();
  $('#version').textContent = 'v' + state.version;
  renderSettings();
  renderMcp();
  renderMcpLog(state.mcpLog || []);
  await renderHistory();
}
boot();

window.shotik.onHistoryChanged(() => renderHistory());
window.shotik.onMcpLog((entry) => prependLog(entry));

/* ---------- capture buttons ---------- */
$('#btnRegion').addEventListener('click', () => window.shotik.captureRegion());
$('#btnFull').addEventListener('click', () => window.shotik.captureFull());
$('#btnRepeat').addEventListener('click', () => window.shotik.captureRepeat());
$('#btnOpenDir').addEventListener('click', () => window.shotik.openSaveDir());

/* ---------- history ---------- */
const SOURCE_LABELS = { region: 'область', fullscreen: 'экран', repeat: 'повтор', mcp: 'Claude', pin: 'pin' };

const ICONS = {
  copy: '<svg viewBox="0 0 24 24"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 012-2h10"/></svg>',
  path: '<svg viewBox="0 0 24 24"><path d="M4 17l6-6-6-6"/><path d="M12 19h8"/></svg>',
  pin: '<svg viewBox="0 0 24 24"><path d="M12 17v5"/><path d="M9 4h6l.8 6.2L18 13H6l2.2-2.8L9 4z"/></svg>',
  ocr: '<svg viewBox="0 0 24 24"><path d="M3 7V5a2 2 0 012-2h2"/><path d="M17 3h2a2 2 0 012 2v2"/><path d="M21 17v2a2 2 0 01-2 2h-2"/><path d="M7 21H5a2 2 0 01-2-2v-2"/><path d="M7 8h10"/><path d="M7 12h6"/><path d="M7 16h8"/></svg>',
  folder: '<svg viewBox="0 0 24 24"><path d="M3 7a2 2 0 012-2h4l2 2.5h8a2 2 0 012 2V17a2 2 0 01-2 2H5a2 2 0 01-2-2V7z"/></svg>',
  trash: '<svg viewBox="0 0 24 24"><path d="M4 7h16"/><path d="M10 11v6"/><path d="M14 11v6"/><path d="M6 7l1 13a1 1 0 001 1h8a1 1 0 001-1l1-13"/><path d="M9 7V4a1 1 0 011-1h4a1 1 0 011 1v3"/></svg>',
};

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

function timeAgo(ts) {
  const s = Math.floor((Date.now() - ts) / 1000);
  if (s < 60) return 'только что';
  if (s < 3600) return Math.floor(s / 60) + ' мин назад';
  if (s < 86400) return Math.floor(s / 3600) + ' ч назад';
  const d = new Date(ts);
  return d.toLocaleDateString('ru-RU', { day: 'numeric', month: 'short' }) + ' ' +
    d.toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit' });
}

async function renderHistory() {
  const items = await window.shotik.historyList();
  const gal = $('#gallery');
  $('#emptyState').hidden = items.length > 0;
  gal.innerHTML = '';
  for (const it of items) {
    const card = document.createElement('div');
    card.className = 'shot';
    card.innerHTML = `
      <div class="shot-thumb">
        <span class="badge">${esc(SOURCE_LABELS[it.source] || it.source)}</span>
        <div class="shot-actions">
          <button class="sa-btn" data-act="copy" data-tip="Копировать">${ICONS.copy}</button>
          <button class="sa-btn" data-act="path" data-tip="Путь">${ICONS.path}</button>
          <button class="sa-btn" data-act="pin" data-tip="Закрепить">${ICONS.pin}</button>
          <button class="sa-btn" data-act="ocr" data-tip="OCR">${ICONS.ocr}</button>
          <button class="sa-btn" data-act="folder" data-tip="В папке">${ICONS.folder}</button>
          <button class="sa-btn danger" data-act="del" data-tip="Удалить">${ICONS.trash}</button>
        </div>
      </div>
      <div class="shot-meta">
        <span class="shot-name">${esc(timeAgo(it.ts))}</span>
        <span class="shot-size">${Number(it.width)}×${Number(it.height)}</span>
      </div>`;
    const thumbUrl = encodeURI('file:///' + String(it.thumb || it.file).replace(/\\/g, '/')).replace(/['()]/g, (c) => '%' + c.charCodeAt(0).toString(16));
    card.querySelector('.shot-thumb').style.backgroundImage = `url("${thumbUrl}")`;
    card.querySelector('.shot-thumb').addEventListener('click', (e) => {
      if (e.target.closest('.sa-btn')) return;
      window.shotik.historyOpen(it.id);
    });
    card.querySelectorAll('.sa-btn').forEach((b) => {
      b.addEventListener('click', async (e) => {
        e.stopPropagation();
        const act = b.dataset.act;
        if (act === 'copy') await window.shotik.historyCopy(it.id);
        if (act === 'path') await window.shotik.historyCopyPath(it.id);
        if (act === 'pin') await window.shotik.historyPin(it.id);
        if (act === 'ocr') {
          b.style.opacity = '0.4';
          await window.shotik.historyOcr(it.id);
          b.style.opacity = '';
        }
        if (act === 'folder') await window.shotik.historyReveal(it.id);
        if (act === 'del') { await window.shotik.historyRemove(it.id); renderHistory(); }
      });
    });
    gal.appendChild(card);
  }
}

/* ---------- claude / mcp ---------- */
function renderMcp() {
  const { mcp } = state;
  const st = state.settings.mcp;
  $('#mcpDot').className = 'dot ' + (mcp.running ? 'on' : 'off');
  $('#mcpEnabled').checked = st.enabled;
  $('#mcpStatusText').innerHTML = mcp.running
    ? `Работает на <b>http://127.0.0.1:${mcp.port}/mcp</b>`
    : (st.enabled ? 'Не запустился — возможно, порт занят' : 'Выключен');
  $('#mcpCmd').textContent = `claude mcp add shotik --transport http http://127.0.0.1:${st.port}/mcp`;
  $('#setPort').value = st.port;
}

$('#mcpEnabled').addEventListener('change', async (e) => {
  await applySettings({ mcp: { enabled: e.target.checked } });
});

$('#btnCopyCmd').addEventListener('click', () => {
  navigator.clipboard.writeText($('#mcpCmd').textContent);
  const b = $('#btnCopyCmd');
  b.textContent = 'Скопировано ✓';
  setTimeout(() => { b.textContent = 'Копировать'; }, 1500);
});

function fmtTime(ts) {
  return new Date(ts).toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function logItemHtml(e) {
  return `<span class="t">${esc(fmtTime(e.ts))}</span><span class="name">${esc(e.tool)}</span>` +
    `<span class="ms">${Number(e.ms)} мс</span><span class="${e.ok ? 'ok' : 'err'}">${e.ok ? '✓' : '✕'}</span>`;
}

function renderMcpLog(entries) {
  const log = $('#mcpLog');
  if (!entries.length) return;
  log.innerHTML = '';
  for (const e of entries) {
    const div = document.createElement('div');
    div.className = 'log-item';
    div.innerHTML = logItemHtml(e);
    log.appendChild(div);
  }
}

function prependLog(entry) {
  const log = $('#mcpLog');
  const empty = log.querySelector('.log-empty');
  if (empty) empty.remove();
  const div = document.createElement('div');
  div.className = 'log-item';
  div.innerHTML = logItemHtml(entry);
  log.prepend(div);
  while (log.children.length > 60) log.lastChild.remove();
}

/* ---------- settings ---------- */
function renderSettings() {
  const s = state.settings;
  $('#saveDirLabel').textContent = s.saveDir;
  $('#setAutoSave').checked = s.autoSave;
  $('#setSmartClip').checked = s.smartClipboard;
  $('#setToast').checked = s.showToast;
  $('#setDownscale').checked = s.downscaleForAI;
  $('#setAutostart').checked = s.launchAtStartup;
  $('#hkRegion').value = s.hotkeys.region || '';
  $('#hkFull').value = s.hotkeys.fullscreen || '';
  $('#hkRepeat').value = s.hotkeys.repeatLast || '';
  $('#kbdRegion').textContent = short(s.hotkeys.region);
  $('#kbdFull').textContent = short(s.hotkeys.fullscreen);
  $('#kbdRepeat').textContent = short(s.hotkeys.repeatLast);
  renderHotkeyWarn();
}

function short(acc) { return (acc || '—').replace('PrintScreen', 'PrtSc'); }

function renderHotkeyWarn() {
  const warn = $('#hotkeyWarn');
  if (state.hotkeyErrors && state.hotkeyErrors.length) {
    warn.hidden = false;
    warn.textContent = '⚠ Не удалось зарегистрировать: ' +
      state.hotkeyErrors.map((h) => h.acc).join(', ') + ' — сочетание занято другим приложением.';
  } else warn.hidden = true;
}

async function applySettings(patch) {
  const res = await window.shotik.setSettings(patch);
  state.settings = res.settings;
  state.mcp = res.mcp;
  state.hotkeyErrors = res.hotkeyErrors;
  renderSettings();
  renderMcp();
}

$('#setAutoSave').addEventListener('change', (e) => applySettings({ autoSave: e.target.checked }));
$('#setSmartClip').addEventListener('change', (e) => applySettings({ smartClipboard: e.target.checked }));
$('#setToast').addEventListener('change', (e) => applySettings({ showToast: e.target.checked }));
$('#setDownscale').addEventListener('change', (e) => applySettings({ downscaleForAI: e.target.checked }));
$('#setAutostart').addEventListener('change', (e) => applySettings({ launchAtStartup: e.target.checked }));
$('#setPort').addEventListener('change', (e) => {
  const p = Math.max(1024, Math.min(65535, Number(e.target.value) || 7464));
  applySettings({ mcp: { port: p } });
});

$('#btnChooseDir').addEventListener('click', async () => {
  const dir = await window.shotik.chooseDir();
  if (dir) { state.settings.saveDir = dir; $('#saveDirLabel').textContent = dir; }
});

/* hotkey recorder */
const HK_FIELDS = [
  ['#hkRegion', 'region'],
  ['#hkFull', 'fullscreen'],
  ['#hkRepeat', 'repeatLast'],
];
for (const [selr, key] of HK_FIELDS) {
  const input = $(selr);
  input.addEventListener('focus', () => {
    input.classList.add('recording');
    input.value = '';
    input.placeholder = 'жми клавиши… (Esc — отмена, Del — убрать)';
  });
  input.addEventListener('blur', () => {
    input.classList.remove('recording');
    input.value = state.settings.hotkeys[key] || '';
    input.placeholder = 'нажми сочетание';
  });
  input.addEventListener('keydown', async (e) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.key === 'Escape') { input.blur(); return; }
    if (e.key === 'Delete' || e.key === 'Backspace') {
      await applySettings({ hotkeys: { [key]: '' } });
      input.blur();
      return;
    }
    const acc = toAccelerator(e);
    if (!acc) return; // modifier-only press — wait for the real key
    await applySettings({ hotkeys: { [key]: acc } });
    input.blur();
  });
}

function toAccelerator(e) {
  const mods = [];
  if (e.ctrlKey) mods.push('Ctrl');
  if (e.altKey) mods.push('Alt');
  if (e.shiftKey) mods.push('Shift');
  if (e.metaKey) mods.push('Super');
  if (['Control', 'Alt', 'Shift', 'Meta'].includes(e.key)) return null;

  let key = null;
  const code = e.code; // layout-independent
  if (/^Key[A-Z]$/.test(code)) key = code.slice(3);
  else if (/^Digit\d$/.test(code)) key = code.slice(5);
  else if (/^F\d+$/.test(code)) key = code;
  else {
    const map = {
      PrintScreen: 'PrintScreen', Space: 'Space', Home: 'Home', End: 'End',
      PageUp: 'PageUp', PageDown: 'PageDown', Insert: 'Insert',
      ArrowUp: 'Up', ArrowDown: 'Down', ArrowLeft: 'Left', ArrowRight: 'Right',
      Backquote: '`', Minus: '-', Equal: '=',
    };
    key = map[code] || null;
  }
  if (!key) return null;
  // PrintScreen / F-keys can be used alone; other keys need a modifier
  if (!mods.length && key !== 'PrintScreen' && !/^F\d+$/.test(key)) return null;
  return [...mods, key].join('+');
}
