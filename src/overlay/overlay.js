'use strict';
/* Shotik overlay: freeze-frame, region selection, annotations, action toolbar. */

const params = new URLSearchParams(location.search);
const IMG_URL = params.get('img');
const DISPLAY_ID = Number(params.get('displayId'));
const FOR_CLAUDE = params.get('claude') === '1';
const FOR_TEXT = params.get('text') === '1';
const FOR_RECORD = params.get('record') === '1';

const cv = document.getElementById('cv');
const ctx = cv.getContext('2d');
const magnifier = document.getElementById('magnifier');
const magCv = document.getElementById('magCv');
const magCtx = magCv.getContext('2d');
const magHex = document.getElementById('magHex');
const magPos = document.getElementById('magPos');
const dimsEl = document.getElementById('dims');
const hintEl = document.getElementById('hint');
const toolbar = document.getElementById('toolbar');
const optionsBar = document.getElementById('optionsBar');
const textEditor = document.getElementById('textEditor');
const claudeBadge = document.getElementById('claudeBadge');
const textLayer = document.getElementById('textLayer');
const textToolbar = document.getElementById('textToolbar');
const translateCard = document.getElementById('translateCard');
let I18N = {};

const COLORS = ['#FF4D5E', '#FF9F2E', '#FFE03A', '#3DDC84', '#3E9EFF', '#B36BFF', '#FF6BD5', '#FFFFFF', '#16181D'];
const STROKES = { S: 2.2, M: 4, L: 7 };       // css px
const FONTS = { S: 16, M: 23, L: 32 };        // css px
const DRAW_TOOLS = ['pen', 'arrow', 'line', 'rect', 'ellipse', 'highlight', 'blur', 'text', 'counter'];
const NEED_SIZE = ['pen', 'arrow', 'line', 'rect', 'ellipse', 'highlight', 'text'];
const NEED_COLOR = ['pen', 'arrow', 'line', 'rect', 'ellipse', 'highlight', 'text', 'counter'];

let ACCENT = '#0067C0';        // OS accent color, fetched at boot

/* localized chrome: hint chips, tooltips, badge */
window.shotik.getI18n().then(({ dict }) => {
  I18N = dict;
  const d = (k) => dict[k] || k;
  document.getElementById('btnTextCopy').setAttribute('data-tip', d('txtCopy'));
  document.getElementById('btnTextAll').setAttribute('data-tip', d('txtAll'));
  document.getElementById('btnTextTranslate').setAttribute('data-tip', d('txtTranslate'));
  document.getElementById('btnTextClose').setAttribute('data-tip', d('txtBack'));
  document.getElementById('translateTitle').textContent = d('txtTranslation');
  document.getElementById('translateCopy').textContent = d('txtCopyTranslation');
  hintEl.innerHTML = '';
  for (const [b, desc] of [
    ['hintDrag', 'hintDragDesc'], ['hintClick', 'hintClickDesc'], ['hintAlt', 'hintAltDesc'],
    ['hintColor', 'hintColorDesc'], ['hintEsc', 'hintEscDesc'],
  ]) {
    const chip = document.createElement('span');
    chip.className = 'chip';
    const bb = document.createElement('b');
    bb.textContent = d(b);
    chip.appendChild(bb);
    chip.appendChild(document.createTextNode(' — ' + d(desc)));
    hintEl.appendChild(chip);
  }
  claudeBadge.childNodes.forEach((n) => { if (n.nodeType === 3) n.textContent = ''; });
  claudeBadge.appendChild(document.createTextNode(' ' + d('claudeBadge')));
  const tips = {
    move: 'toolMove', pen: 'toolPen', arrow: 'toolArrow', line: 'toolLine', rect: 'toolRect',
    ellipse: 'toolEllipse', highlight: 'toolHighlight', blur: 'toolBlur', text: 'toolText', counter: 'toolCounter',
  };
  document.querySelectorAll('#toolButtons .tb-btn').forEach((b) => b.setAttribute('data-tip', d(tips[b.dataset.tool])));
  const byId = {
    btnUndo: 'tipUndo', btnRedo: 'tipRedo', btnOcr: 'tipOcr', btnPin: 'tipPin', btnRec: 'tipRec',
    btnSave: 'tipSave', btnClaude: 'tipClaude', btnCopy: 'tipCopy', btnCancel: 'tipCancel',
  };
  for (const [id, key] of Object.entries(byId)) document.getElementById(id).setAttribute('data-tip', d(key));
  const sizeTips = { S: 'tipThin', M: 'tipMedium', L: 'tipThick' };
  document.querySelectorAll('.size-btn').forEach((b) => b.setAttribute('data-tip', d(sizeTips[b.dataset.size])));
});

window.shotik.getTheme().then((t) => {
  ACCENT = t.accent;
  const n = parseInt(t.accent.slice(1), 16);
  const lum = 0.299 * ((n >> 16) & 255) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255);
  document.documentElement.style.setProperty('--accent', t.accent);
  document.documentElement.style.setProperty('--accent-text', lum > 160 ? '#1b1b1b' : '#ffffff');
  scheduleRender();
});

let img = new Image();
let srcCv, srcCtx;             // pristine screenshot (image px)
let imgW = 0, imgH = 0, kx = 1, ky = 1;
let loaded = false;

let mode = 'idle';             // idle | selecting | selected
let sel = null;                // {x,y,w,h} image px
let snapWindows = [];          // window rects under this display (css px, topmost first)
let hoverRect = null;          // currently highlighted window (css px)
let altDown = false;           // Alt suppresses window snapping
let shiftDown = false;         // Shift suppresses snapping too; Shift+click = whole screen
let drag = null;
let tool = 'move';
let color = COLORS[0];
let sizeKey = 'M';
let annotations = [];
let redoStack = [];
let counterN = 1;
let tempShape = null;
let mouse = { x: -100, y: -100 };
let renderQueued = false;
let textPending = null;        // {imgX, imgY}

// Live Text mode
let textMode = false;
let words = [];                // [{ text, line, x, y, w, h }] in css px
let selWords = new Set();      // selected word indices
let rubber = null;             // { sx, sy } css during rubber-band select
let rubberEl = null;

/* ============================ geometry helpers ============================ */
const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
const toImg = (cx_, cy_) => ({ x: clamp(cx_ * kx, 0, imgW), y: clamp(cy_ * ky, 0, imgH) });
const selCss = () => sel && { x: sel.x / kx, y: sel.y / ky, w: sel.w / kx, h: sel.h / ky };
const normRect = (x1, y1, x2, y2) => ({
  x: Math.min(x1, x2), y: Math.min(y1, y2),
  w: Math.abs(x2 - x1), h: Math.abs(y2 - y1),
});
const roundSel = () => ({
  x: Math.round(sel.x), y: Math.round(sel.y),
  w: Math.max(1, Math.round(sel.w)), h: Math.max(1, Math.round(sel.h)),
});

/* ============================ boot ============================ */
img.onload = () => {
  imgW = img.naturalWidth; imgH = img.naturalHeight;
  cv.width = imgW; cv.height = imgH;
  kx = imgW / window.innerWidth; ky = imgH / window.innerHeight;
  srcCv = document.createElement('canvas');
  srcCv.width = imgW; srcCv.height = imgH;
  srcCtx = srcCv.getContext('2d', { willReadFrequently: true });
  srcCtx.drawImage(img, 0, 0);
  loaded = true;
  hintEl.hidden = false;
  if (FOR_CLAUDE) claudeBadge.hidden = false;
  scheduleRender();
};
img.src = IMG_URL;

window.shotik.onWindows((list) => {
  snapWindows = list;
  updateHover();
});

function updateHover() {
  const prev = hoverRect;
  hoverRect = null;
  if (mode === 'idle' && !altDown && !shiftDown && mouse.x >= 0) {
    const hit = snapWindows.find((r) =>
      mouse.x >= r.x && mouse.x <= r.x + r.w && mouse.y >= r.y && mouse.y <= r.y + r.h);
    if (hit) {
      // clip to the viewport
      const x1 = Math.max(0, hit.x), y1 = Math.max(0, hit.y);
      const x2 = Math.min(window.innerWidth, hit.x + hit.w);
      const y2 = Math.min(window.innerHeight, hit.y + hit.h);
      if (x2 - x1 > 8 && y2 - y1 > 8) hoverRect = { x: x1, y: y1, w: x2 - x1, h: y2 - y1 };
    }
  }
  if (JSON.stringify(prev) !== JSON.stringify(hoverRect)) scheduleRender();
}

/* color palette buttons */
const colorRow = document.getElementById('colorRow');
for (const c of COLORS) {
  const b = document.createElement('button');
  b.className = 'color-btn' + (c === color ? ' active' : '');
  b.style.background = c;
  b.addEventListener('mousedown', (e) => { e.stopPropagation(); });
  b.addEventListener('click', () => {
    color = c;
    colorRow.querySelectorAll('.color-btn').forEach((x) => x.classList.remove('active'));
    b.classList.add('active');
    if (!textEditor.hidden) textEditor.style.color = color;
  });
  colorRow.appendChild(b);
}

/* ============================ rendering ============================ */
function scheduleRender() {
  if (renderQueued) return;
  renderQueued = true;
  requestAnimationFrame(() => { renderQueued = false; render(); });
}

function render() {
  if (!loaded) return;
  ctx.clearRect(0, 0, imgW, imgH);
  ctx.drawImage(srcCv, 0, 0);
  for (const a of annotations) drawAnno(ctx, a, 0, 0);
  if (tempShape) drawAnno(ctx, tempShape, 0, 0);

  // veil over non-selected area
  ctx.fillStyle = 'rgba(6, 8, 13, 0.45)';
  if (sel && (mode === 'selected' || mode === 'selecting')) {
    const r = roundSel();
    ctx.fillRect(0, 0, imgW, r.y);
    ctx.fillRect(0, r.y, r.x, r.h);
    ctx.fillRect(r.x + r.w, r.y, imgW - r.x - r.w, r.h);
    ctx.fillRect(0, r.y + r.h, imgW, imgH - r.y - r.h);
    drawSelectionChrome(r);
  } else if (mode === 'idle' && hoverRect) {
    // window under cursor: un-dimmed + accent outline
    const r = {
      x: Math.round(hoverRect.x * kx), y: Math.round(hoverRect.y * ky),
      w: Math.round(hoverRect.w * kx), h: Math.round(hoverRect.h * ky),
    };
    ctx.fillRect(0, 0, imgW, r.y);
    ctx.fillRect(0, r.y, r.x, r.h);
    ctx.fillRect(r.x + r.w, r.y, imgW - r.x - r.w, r.h);
    ctx.fillRect(0, r.y + r.h, imgW, imgH - r.y - r.h);
    ctx.save();
    ctx.strokeStyle = ACCENT;
    ctx.lineWidth = Math.max(2, 2 * kx);
    ctx.strokeRect(r.x + ctx.lineWidth / 2, r.y + ctx.lineWidth / 2, r.w - ctx.lineWidth, r.h - ctx.lineWidth);
    ctx.restore();
  } else {
    ctx.fillRect(0, 0, imgW, imgH);
  }
  updateDom();
}

function drawSelectionChrome(r) {
  const lw = Math.max(1.5, 1.5 * kx);
  ctx.save();
  // border
  ctx.strokeStyle = ACCENT;
  ctx.lineWidth = lw;
  ctx.strokeRect(r.x - lw / 2, r.y - lw / 2, r.w + lw, r.h + lw);
  // rule-of-thirds (subtle, only for medium+ selections)
  if (r.w / kx > 150 && r.h / ky > 150 && (drag && (drag.kind === 'create' || drag.kind === 'resize' || drag.kind === 'move'))) {
    ctx.strokeStyle = 'rgba(255,255,255,0.13)';
    ctx.lineWidth = Math.max(1, kx);
    for (let i = 1; i <= 2; i++) {
      ctx.beginPath();
      ctx.moveTo(r.x + (r.w * i) / 3, r.y); ctx.lineTo(r.x + (r.w * i) / 3, r.y + r.h);
      ctx.moveTo(r.x, r.y + (r.h * i) / 3); ctx.lineTo(r.x + r.w, r.y + (r.h * i) / 3);
      ctx.stroke();
    }
  }
  // handles
  if (mode === 'selected' && tool === 'move' && !textMode && (!drag || drag.kind !== 'create')) {
    const hs = 7 * kx;
    ctx.fillStyle = '#ffffff';
    ctx.strokeStyle = ACCENT;
    ctx.lineWidth = Math.max(1.2, 1.2 * kx);
    for (const [hx, hy] of handlePoints(r)) {
      ctx.beginPath();
      ctx.arc(hx, hy, hs / 2 + ctx.lineWidth / 2, 0, Math.PI * 2);
      ctx.fill(); ctx.stroke();
    }
  }
  ctx.restore();
}

function handlePoints(r) {
  return [
    [r.x, r.y], [r.x + r.w / 2, r.y], [r.x + r.w, r.y],
    [r.x + r.w, r.y + r.h / 2], [r.x + r.w, r.y + r.h],
    [r.x + r.w / 2, r.y + r.h], [r.x, r.y + r.h], [r.x, r.y + r.h / 2],
  ];
}
const HANDLE_NAMES = ['nw', 'n', 'ne', 'e', 'se', 's', 'sw', 'w'];
const HANDLE_CURSORS = {
  nw: 'nwse-resize', se: 'nwse-resize', ne: 'nesw-resize', sw: 'nesw-resize',
  n: 'ns-resize', s: 'ns-resize', e: 'ew-resize', w: 'ew-resize',
};

function handleAt(cssX, cssY) {
  if (!sel || mode !== 'selected' || tool !== 'move') return null;
  const r = selCss();
  const pts = [
    [r.x, r.y], [r.x + r.w / 2, r.y], [r.x + r.w, r.y],
    [r.x + r.w, r.y + r.h / 2], [r.x + r.w, r.y + r.h],
    [r.x + r.w / 2, r.y + r.h], [r.x, r.y + r.h], [r.x, r.y + r.h / 2],
  ];
  for (let i = 0; i < pts.length; i++) {
    if (Math.abs(cssX - pts[i][0]) <= 7 && Math.abs(cssY - pts[i][1]) <= 7) return HANDLE_NAMES[i];
  }
  return null;
}

function insideSel(ix, iy) {
  return sel && ix >= sel.x && ix <= sel.x + sel.w && iy >= sel.y && iy <= sel.y + sel.h;
}

/* ---------- annotation rendering (shared by screen + export) ---------- */
const blurCache = new Map();

function drawAnno(c, a, ox, oy) {
  c.save();
  c.translate(-ox, -oy);
  const lw = (a.stroke || 4) * kx;
  switch (a.type) {
    case 'pen': {
      shadow(c);
      c.strokeStyle = a.color; c.lineWidth = lw; c.lineCap = 'round'; c.lineJoin = 'round';
      c.beginPath();
      const pts = a.points;
      c.moveTo(pts[0].x, pts[0].y);
      for (let i = 1; i < pts.length - 1; i++) {
        const mx = (pts[i].x + pts[i + 1].x) / 2, my = (pts[i].y + pts[i + 1].y) / 2;
        c.quadraticCurveTo(pts[i].x, pts[i].y, mx, my);
      }
      if (pts.length > 1) c.lineTo(pts[pts.length - 1].x, pts[pts.length - 1].y);
      c.stroke();
      break;
    }
    case 'line': case 'arrow': {
      shadow(c);
      c.strokeStyle = a.color; c.fillStyle = a.color;
      c.lineWidth = lw; c.lineCap = 'round';
      c.beginPath(); c.moveTo(a.x1, a.y1); c.lineTo(a.x2, a.y2); c.stroke();
      if (a.type === 'arrow') {
        const ang = Math.atan2(a.y2 - a.y1, a.x2 - a.x1);
        const hl = Math.max(10 * kx, lw * 3.2);
        c.beginPath();
        c.moveTo(a.x2, a.y2);
        c.lineTo(a.x2 - hl * Math.cos(ang - 0.45), a.y2 - hl * Math.sin(ang - 0.45));
        c.lineTo(a.x2 - hl * Math.cos(ang + 0.45), a.y2 - hl * Math.sin(ang + 0.45));
        c.closePath(); c.fill();
      }
      break;
    }
    case 'rect': {
      shadow(c);
      c.strokeStyle = a.color; c.lineWidth = lw;
      const r = normRect(a.x1, a.y1, a.x2, a.y2);
      c.beginPath();
      c.roundRect(r.x, r.y, r.w, r.h, Math.min(6 * kx, r.w / 4, r.h / 4));
      c.stroke();
      break;
    }
    case 'ellipse': {
      shadow(c);
      c.strokeStyle = a.color; c.lineWidth = lw;
      const r = normRect(a.x1, a.y1, a.x2, a.y2);
      c.beginPath();
      c.ellipse(r.x + r.w / 2, r.y + r.h / 2, r.w / 2, r.h / 2, 0, 0, Math.PI * 2);
      c.stroke();
      break;
    }
    case 'highlight': {
      c.globalAlpha = 0.42;
      c.globalCompositeOperation = 'multiply';
      c.strokeStyle = a.color; c.lineWidth = lw * 3.2; c.lineCap = 'butt'; c.lineJoin = 'round';
      c.beginPath();
      const pts = a.points;
      c.moveTo(pts[0].x, pts[0].y);
      for (const p of pts) c.lineTo(p.x, p.y);
      c.stroke();
      break;
    }
    case 'blur': {
      const r = normRect(a.x1, a.y1, a.x2, a.y2);
      if (r.w < 2 || r.h < 2) break;
      const key = `${Math.round(r.x)},${Math.round(r.y)},${Math.round(r.w)},${Math.round(r.h)}`;
      let patch = blurCache.get(key);
      if (!patch) {
        const block = Math.max(8 * kx, Math.min(r.w, r.h) / 12);
        const tw = Math.max(1, Math.round(r.w / block)), th = Math.max(1, Math.round(r.h / block));
        const tiny = document.createElement('canvas');
        tiny.width = tw; tiny.height = th;
        tiny.getContext('2d').drawImage(srcCv, r.x, r.y, r.w, r.h, 0, 0, tw, th);
        patch = document.createElement('canvas');
        patch.width = Math.round(r.w); patch.height = Math.round(r.h);
        const pc = patch.getContext('2d');
        pc.imageSmoothingEnabled = false;
        pc.drawImage(tiny, 0, 0, Math.round(r.w), Math.round(r.h));
        blurCache.set(key, patch);
      }
      c.drawImage(patch, r.x, r.y);
      break;
    }
    case 'text': {
      shadow(c);
      const fp = a.fontPx * kx;
      c.fillStyle = a.color;
      c.font = `700 ${fp}px 'Segoe UI', system-ui, sans-serif`;
      c.textBaseline = 'top';
      a.lines.forEach((ln, i) => c.fillText(ln, a.x, a.y + i * fp * 1.25));
      break;
    }
    case 'counter': {
      shadow(c);
      const r = 15 * kx;
      c.fillStyle = a.color;
      c.beginPath(); c.arc(a.x, a.y, r, 0, Math.PI * 2); c.fill();
      c.shadowColor = 'transparent';
      c.fillStyle = lightOrDark(a.color);
      c.font = `700 ${r * 1.15}px 'Segoe UI', system-ui, sans-serif`;
      c.textAlign = 'center'; c.textBaseline = 'middle';
      c.fillText(String(a.n), a.x, a.y + r * 0.05);
      break;
    }
  }
  c.restore();
}

function shadow(c) {
  c.shadowColor = 'rgba(0,0,0,0.38)';
  c.shadowBlur = 5 * kx;
  c.shadowOffsetY = 1.5 * kx;
}

function lightOrDark(hex) {
  const n = parseInt(hex.slice(1), 16);
  const lum = 0.299 * ((n >> 16) & 255) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255);
  return lum > 150 ? '#1b1d24' : '#ffffff';
}

/* ============================ DOM chrome ============================ */
function updateDom() {
  // dims badge
  if (sel && mode !== 'idle' && !textMode) {
    const r = roundSel(), rc = selCss();
    dimsEl.hidden = false;
    dimsEl.textContent = `${r.w} × ${r.h}`;
    const bx = clamp(rc.x, 8, window.innerWidth - 90);
    let by = rc.y - 30;
    if (by < 6) by = rc.y + 8;
    dimsEl.style.left = bx + 'px';
    dimsEl.style.top = by + 'px';
  } else if (mode === 'idle' && hoverRect) {
    dimsEl.hidden = false;
    dimsEl.textContent = `${Math.round(hoverRect.w * kx)} × ${Math.round(hoverRect.h * ky)}`;
    dimsEl.style.left = clamp(hoverRect.x, 8, window.innerWidth - 90) + 'px';
    dimsEl.style.top = Math.max(6, hoverRect.y - 30) + 'px';
  } else dimsEl.hidden = true;

  // magnifier
  const magOn = loaded && mouse.x >= 0 && (mode === 'idle' || (drag && (drag.kind === 'create' || drag.kind === 'resize')));
  magnifier.hidden = !magOn;
  if (magOn) updateMagnifier();

  hintEl.hidden = !(mode === 'idle');
}

function updateMagnifier() {
  const ix = clamp(Math.round(mouse.x * kx), 0, imgW - 1);
  const iy = clamp(Math.round(mouse.y * ky), 0, imgH - 1);
  magCtx.imageSmoothingEnabled = false;
  magCtx.clearRect(0, 0, 143, 143);
  magCtx.drawImage(srcCv, ix - 6, iy - 6, 13, 13, 0, 0, 143, 143);
  // grid
  magCtx.strokeStyle = 'rgba(255,255,255,0.10)';
  magCtx.lineWidth = 1;
  for (let i = 1; i < 13; i++) {
    magCtx.beginPath(); magCtx.moveTo(i * 11, 0); magCtx.lineTo(i * 11, 143); magCtx.stroke();
    magCtx.beginPath(); magCtx.moveTo(0, i * 11); magCtx.lineTo(143, i * 11); magCtx.stroke();
  }
  // center pixel marker
  magCtx.strokeStyle = ACCENT; magCtx.lineWidth = 2;
  magCtx.strokeRect(66, 66, 11, 11);

  const px = srcCtx.getImageData(ix, iy, 1, 1).data;
  const hex = '#' + [px[0], px[1], px[2]].map((v) => v.toString(16).padStart(2, '0')).join('').toUpperCase();
  magHex.textContent = hex;
  magPos.textContent = `${ix}, ${iy}`;

  let mx = mouse.x + 26, my = mouse.y + 26;
  if (mx + 175 > window.innerWidth) mx = mouse.x - 26 - 159;
  if (my + 205 > window.innerHeight) my = mouse.y - 26 - 192;
  magnifier.style.left = mx + 'px';
  magnifier.style.top = my + 'px';
}

function positionToolbar() {
  if (mode !== 'selected' || textMode) { toolbar.hidden = true; optionsBar.hidden = true; return; }
  toolbar.hidden = false;
  const rc = selCss();
  const tw = toolbar.offsetWidth, th = toolbar.offsetHeight;
  const m = 10;
  let x = clamp(rc.x + rc.w / 2 - tw / 2, 8, window.innerWidth - tw - 8);
  let y = rc.y + rc.h + m;
  if (y + th > window.innerHeight - 8) y = rc.y - th - m;
  if (y < 8) y = Math.max(8, Math.min(window.innerHeight - th - 8, rc.y + rc.h - th - m - 8));
  toolbar.style.left = x + 'px';
  toolbar.style.top = y + 'px';
  toolbar.classList.toggle('tips-below', y < 56);

  const needOpts = NEED_COLOR.includes(tool);
  optionsBar.hidden = !needOpts;
  if (needOpts) {
    const ow = optionsBar.offsetWidth, oh = optionsBar.offsetHeight;
    let oy = y + th + 8;
    if (oy + oh > window.innerHeight - 8) oy = y - oh - 8;
    optionsBar.style.left = clamp(x, 8, window.innerWidth - ow - 8) + 'px';
    optionsBar.style.top = oy + 'px';
    optionsBar.querySelectorAll('.size-btn').forEach((b) => b.classList.toggle('active', b.dataset.size === sizeKey));
  }
  updateUndoButtons();
}

function updateUndoButtons() {
  document.getElementById('btnUndo').disabled = annotations.length === 0;
  document.getElementById('btnRedo').disabled = redoStack.length === 0;
}

/* ============================ interactions ============================ */
function isUiTarget(e) {
  return e.target.closest('#toolbar, #optionsBar, #textEditor, #textToolbar, #translateCard');
}

window.addEventListener('mousedown', (e) => {
  if (!loaded || e.button !== 0) return;
  if (textMode) { if (!isUiTarget(e)) textDown(e); return; }
  if (isUiTarget(e)) return;
  if (!textEditor.hidden) { commitText(); return; }
  const ip = toImg(e.clientX, e.clientY);

  if (mode === 'idle') {
    drag = { kind: 'create', sx: ip.x, sy: ip.y, sCssX: e.clientX, sCssY: e.clientY };
    sel = { x: ip.x, y: ip.y, w: 0, h: 0 };
    mode = 'selecting';
    toolbar.hidden = true; optionsBar.hidden = true;
    scheduleRender();
    return;
  }

  if (mode === 'selected') {
    if (tool === 'move') {
      const h = handleAt(e.clientX, e.clientY);
      if (h) {
        const r = roundSel();
        const anchor = {
          nw: [r.x + r.w, r.y + r.h], ne: [r.x, r.y + r.h], se: [r.x, r.y], sw: [r.x + r.w, r.y],
          n: [null, r.y + r.h], s: [null, r.y], e: [r.x, null], w: [r.x + r.w, null],
        }[h];
        drag = { kind: 'resize', edge: h, ax: anchor[0], ay: anchor[1], r0: r };
        toolbar.hidden = true; optionsBar.hidden = true;
      } else if (insideSel(ip.x, ip.y)) {
        drag = { kind: 'move', offX: ip.x - sel.x, offY: ip.y - sel.y };
        toolbar.hidden = true; optionsBar.hidden = true;
      } else {
        drag = { kind: 'create', sx: ip.x, sy: ip.y, sCssX: e.clientX, sCssY: e.clientY };
        sel = { x: ip.x, y: ip.y, w: 0, h: 0 };
        mode = 'selecting';
        toolbar.hidden = true; optionsBar.hidden = true;
      }
      scheduleRender();
      return;
    }
    // drawing tools
    if (tool === 'text') {
      openTextEditor(e.clientX, e.clientY, ip);
      return;
    }
    if (tool === 'counter') {
      pushAnno({ type: 'counter', x: ip.x, y: ip.y, color, n: counterN++ });
      return;
    }
    drag = { kind: 'draw', sx: ip.x, sy: ip.y };
    if (tool === 'pen' || tool === 'highlight') {
      tempShape = { type: tool, points: [{ x: ip.x, y: ip.y }], color, stroke: STROKES[sizeKey] };
    } else {
      tempShape = { type: tool, x1: ip.x, y1: ip.y, x2: ip.x, y2: ip.y, color, stroke: STROKES[sizeKey] };
    }
    scheduleRender();
  }
});

window.addEventListener('mousemove', (e) => {
  mouse = { x: e.clientX, y: e.clientY };
  if (!loaded) return;
  if (textMode) { textMoveDrag(e); return; }
  const ip = toImg(e.clientX, e.clientY);

  if (drag) {
    switch (drag.kind) {
      case 'create': {
        sel = normRect(drag.sx, drag.sy, ip.x, ip.y);
        break;
      }
      case 'move': {
        sel.x = clamp(ip.x - drag.offX, 0, imgW - sel.w);
        sel.y = clamp(ip.y - drag.offY, 0, imgH - sel.h);
        break;
      }
      case 'resize': {
        const r0 = drag.r0;
        const px = drag.ax === null ? null : ip.x;
        const py = drag.ay === null ? null : ip.y;
        const x1 = drag.ax === null ? r0.x : drag.ax;
        const y1 = drag.ay === null ? r0.y : drag.ay;
        const x2 = px === null ? r0.x + r0.w : px;
        const y2 = py === null ? r0.y + r0.h : py;
        sel = normRect(x1, y1, x2, y2);
        if (drag.ax === null) { sel.x = r0.x; sel.w = r0.w; }
        if (drag.ay === null) { sel.y = r0.y; sel.h = r0.h; }
        break;
      }
      case 'draw': {
        if (tool === 'pen' || tool === 'highlight') {
          const pts = tempShape.points;
          const lastP = pts[pts.length - 1];
          if (Math.hypot(ip.x - lastP.x, ip.y - lastP.y) > 2 * kx) pts.push({ x: ip.x, y: ip.y });
        } else {
          let x2 = ip.x, y2 = ip.y;
          if (e.shiftKey && (tool === 'line' || tool === 'arrow')) {
            // snap to 45°
            const dx = x2 - tempShape.x1, dy = y2 - tempShape.y1;
            const ang = Math.round(Math.atan2(dy, dx) / (Math.PI / 4)) * (Math.PI / 4);
            const len = Math.hypot(dx, dy);
            x2 = tempShape.x1 + len * Math.cos(ang);
            y2 = tempShape.y1 + len * Math.sin(ang);
          }
          if (e.shiftKey && (tool === 'rect' || tool === 'ellipse')) {
            const s = Math.max(Math.abs(ip.x - tempShape.x1), Math.abs(ip.y - tempShape.y1));
            x2 = tempShape.x1 + Math.sign(ip.x - tempShape.x1 || 1) * s;
            y2 = tempShape.y1 + Math.sign(ip.y - tempShape.y1 || 1) * s;
          }
          tempShape.x2 = x2; tempShape.y2 = y2;
        }
        break;
      }
    }
    scheduleRender();
    return;
  }

  // cursor feedback
  if (mode === 'selected' && tool === 'move') {
    const h = handleAt(e.clientX, e.clientY);
    document.body.style.cursor = h ? HANDLE_CURSORS[h] : (insideSel(ip.x, ip.y) ? 'move' : 'crosshair');
  } else if (mode === 'selected' && tool === 'text') {
    document.body.style.cursor = 'text';
  } else {
    document.body.style.cursor = 'crosshair';
  }
  if (mode === 'idle') {
    altDown = e.altKey;
    shiftDown = e.shiftKey;
    updateHover();
    scheduleRender();
  }
});

window.addEventListener('mouseup', (e) => {
  if (!loaded || e.button !== 0) return;
  if (textMode) { textUp(e); return; }
  if (!drag) return;
  const d = drag;
  drag = null;

  if (d.kind === 'create') {
    const moved = Math.hypot(e.clientX - d.sCssX, e.clientY - d.sCssY);
    if (moved < 4) {
      // click: Shift = whole screen; otherwise snap to the hovered window, else whole screen
      if (!e.shiftKey && hoverRect) {
        sel = {
          x: hoverRect.x * kx, y: hoverRect.y * ky,
          w: hoverRect.w * kx, h: hoverRect.h * ky,
        };
      } else {
        sel = { x: 0, y: 0, w: imgW, h: imgH };
      }
      hoverRect = null;
    } else if (sel.w < 3 || sel.h < 3) {
      sel = null; mode = 'idle'; scheduleRender(); return;
    }
    mode = 'selected';
    scheduleRender();
    if (FOR_RECORD) { // record hotkey: select → start recording immediately
      const r = roundSel();
      window.shotik.action({ action: 'record', displayId: DISPLAY_ID, rectPhys: r, rectCss: { x: r.x / kx, y: r.y / ky, w: r.w / kx, h: r.h / ky } });
      return;
    }
    if (FOR_TEXT) { enterTextMode(); return; } // text-grab hotkey: straight into Live Text
    requestAnimationFrame(positionToolbar);
    return;
  }
  if (d.kind === 'draw') {
    if (tempShape) {
      const significant = (tempShape.points && tempShape.points.length > 1) ||
        (tempShape.x1 !== undefined && (Math.abs(tempShape.x2 - tempShape.x1) > 3 || Math.abs(tempShape.y2 - tempShape.y1) > 3));
      if (significant) pushAnno(tempShape);
      tempShape = null;
    }
    scheduleRender();
    return;
  }
  // move / resize finished
  scheduleRender();
  requestAnimationFrame(positionToolbar);
});

window.addEventListener('dblclick', (e) => {
  if (isUiTarget(e) || mode !== 'selected') return;
  const ip = toImg(e.clientX, e.clientY);
  if (insideSel(ip.x, ip.y)) doAction('copy');
});

window.addEventListener('contextmenu', (e) => e.preventDefault());

function pushAnno(a) {
  annotations.push(a);
  redoStack = [];
  scheduleRender();
  updateUndoButtons();
}

/* ============================ Live Text mode ============================ */
const td = (k) => I18N[k] || k;

async function enterTextMode() {
  if (textMode || mode !== 'selected' || !sel) return;
  textMode = true;
  toolbar.hidden = true; optionsBar.hidden = true; hintEl.hidden = true;
  claudeBadge.hidden = true;
  document.body.style.cursor = 'progress';
  scheduleRender();
  flash(td('txtRecognizing'));

  // OCR the current selection crop
  const r = roundSel();
  const oc = document.createElement('canvas');
  oc.width = r.w; oc.height = r.h;
  oc.getContext('2d').drawImage(srcCv, r.x, r.y, r.w, r.h, 0, 0, r.w, r.h);
  const blob = await new Promise((res) => oc.toBlob(res, 'image/png'));
  const buf = new Uint8Array(await blob.arrayBuffer());
  let data = { lines: [] };
  try { data = await window.shotik.ocrBoxes(buf); } catch (_) {}

  document.body.style.cursor = 'text';
  // map crop-px word boxes -> css, keeping line grouping
  words = [];
  (data.lines || []).forEach((ln, li) => {
    (ln.words || []).forEach((w) => {
      words.push({
        text: w.text, line: li,
        x: (r.x + w.x) / kx, y: (r.y + w.y) / ky,
        w: w.w / kx, h: w.h / ky,
      });
    });
  });
  selWords = new Set();
  renderWords();
  if (!words.length) flash(td('ocrNoText'));
  positionTextToolbar();
}

function exitTextMode() {
  textMode = false;
  textLayer.hidden = true; textLayer.innerHTML = '';
  textToolbar.hidden = true; translateCard.hidden = true;
  words = []; selWords = new Set(); rubber = null;
  if (rubberEl) { rubberEl.remove(); rubberEl = null; }
  document.body.style.cursor = 'crosshair';
  scheduleRender();
  requestAnimationFrame(positionToolbar);
}

function renderWords() {
  textLayer.hidden = false;
  textLayer.innerHTML = '';
  words.forEach((w, i) => {
    const el = document.createElement('div');
    el.className = 'word' + (selWords.has(i) ? ' sel' : '');
    el.style.left = w.x + 'px'; el.style.top = w.y + 'px';
    el.style.width = w.w + 'px'; el.style.height = w.h + 'px';
    el.dataset.i = i;
    textLayer.appendChild(el);
  });
}

function wordAt(cx, cy) {
  for (let i = words.length - 1; i >= 0; i--) {
    const w = words[i];
    if (cx >= w.x && cx <= w.x + w.w && cy >= w.y && cy <= w.y + w.h) return i;
  }
  return -1;
}

function textDown(e) {
  translateCard.hidden = true;
  const i = wordAt(e.clientX, e.clientY);
  if (i >= 0 && !e.shiftKey) {
    // start from a word — select it, allow drag to extend by rubber band
    selWords = new Set([i]);
  } else if (!e.shiftKey) {
    selWords = new Set();
  }
  rubber = { sx: e.clientX, sy: e.clientY, base: new Set(selWords) };
  renderWords();
}

function textMoveDrag(e) {
  if (!rubber) return;
  const x = Math.min(rubber.sx, e.clientX), y = Math.min(rubber.sy, e.clientY);
  const w = Math.abs(e.clientX - rubber.sx), h = Math.abs(e.clientY - rubber.sy);
  if (!rubberEl) { rubberEl = document.createElement('div'); rubberEl.id = 'selRubber'; document.body.appendChild(rubberEl); }
  rubberEl.style.left = x + 'px'; rubberEl.style.top = y + 'px';
  rubberEl.style.width = w + 'px'; rubberEl.style.height = h + 'px';
  const next = new Set(rubber.base);
  words.forEach((wd, i) => {
    if (wd.x < x + w && wd.x + wd.w > x && wd.y < y + h && wd.y + wd.h > y) next.add(i);
  });
  selWords = next;
  renderWords();
}

function textUp() {
  rubber = null;
  if (rubberEl) { rubberEl.remove(); rubberEl = null; }
  positionTextToolbar();
}

function selectedText() {
  const idx = selWords.size ? [...selWords] : words.map((_, i) => i); // nothing selected → all
  const byLine = new Map();
  idx.forEach((i) => {
    const w = words[i];
    if (!byLine.has(w.line)) byLine.set(w.line, []);
    byLine.get(w.line).push(w);
  });
  return [...byLine.keys()].sort((a, b) => a - b)
    .map((li) => byLine.get(li).sort((a, b) => a.x - b.x).map((w) => w.text).join(' '))
    .join('\n');
}

function positionTextToolbar() {
  if (!textMode) return;
  textToolbar.hidden = false;
  const rc = selCss();
  const tw = textToolbar.offsetWidth, th = textToolbar.offsetHeight, m = 10;
  let x = clamp(rc.x + rc.w / 2 - tw / 2, 8, window.innerWidth - tw - 8);
  let y = rc.y + rc.h + m;
  if (y + th > window.innerHeight - 8) y = Math.max(8, rc.y - th - m);
  textToolbar.style.left = x + 'px';
  textToolbar.style.top = y + 'px';
}

let flashTimer = null;
function flash(msg) {
  let el = document.getElementById('textFlash');
  if (!el) { el = document.createElement('div'); el.id = 'textFlash'; document.body.appendChild(el); }
  el.textContent = msg;
  const rc = selCss() || { x: window.innerWidth / 2, y: 40, w: 0 };
  el.style.left = clamp(rc.x + rc.w / 2, 80, window.innerWidth - 80) + 'px';
  el.style.top = Math.max(20, rc.y - 40) + 'px';
  el.hidden = false;
  clearTimeout(flashTimer);
  flashTimer = setTimeout(() => { el.hidden = true; }, 1400);
}

function copyTextSelection(close) {
  const text = selectedText();
  if (!text) { flash(td('ocrNoText')); return; }
  if (close) {
    window.shotik.action({ action: 'copytext', text, displayId: DISPLAY_ID });
  } else {
    navigator.clipboard.writeText(text);
    flash(td('copiedTitle'));
  }
}

async function translateSelection() {
  const text = selectedText();
  if (!text) { flash(td('ocrNoText')); return; }
  translateCard.hidden = false;
  const body = document.getElementById('translateBody');
  const meta = document.getElementById('translateMeta');
  body.classList.add('loading');
  body.textContent = td('txtTranslating');
  meta.textContent = '';
  positionTranslateCard();
  try {
    const res = await window.shotik.translate(text);
    body.classList.remove('loading');
    body.textContent = res.text || '—';
    meta.textContent = res.source && res.source !== 'auto' ? (res.source + ' → ' + (res.target || '')) : '';
  } catch (e) {
    body.classList.remove('loading');
    body.textContent = td('txtTranslateFail');
  }
  positionTranslateCard();
}

function positionTranslateCard() {
  const rc = selCss();
  const cw = translateCard.offsetWidth, ch = translateCard.offsetHeight, m = 12;
  let x = clamp(rc.x + rc.w + m, 8, window.innerWidth - cw - 8);
  if (rc.x + rc.w + m + cw > window.innerWidth) x = clamp(rc.x - cw - m, 8, window.innerWidth - cw - 8);
  let y = clamp(rc.y, 8, window.innerHeight - ch - 8);
  translateCard.style.left = x + 'px';
  translateCard.style.top = y + 'px';
}

// text toolbar wiring
textToolbar.addEventListener('mousedown', (e) => e.stopPropagation());
translateCard.addEventListener('mousedown', (e) => e.stopPropagation());
document.getElementById('btnTextCopy').addEventListener('click', () => copyTextSelection(true));
document.getElementById('btnTextAll').addEventListener('click', () => { selWords = new Set(words.map((_, i) => i)); renderWords(); });
document.getElementById('btnTextTranslate').addEventListener('click', () => translateSelection());
document.getElementById('btnTextClose').addEventListener('click', () => exitTextMode());
document.getElementById('translateClose').addEventListener('click', () => { translateCard.hidden = true; });
document.getElementById('translateCopy').addEventListener('click', () => {
  navigator.clipboard.writeText(document.getElementById('translateBody').textContent);
  flash(td('copiedTitle'));
});

/* ============================ text editor ============================ */
function openTextEditor(cssX, cssY, ip) {
  textPending = { imgX: ip.x, imgY: ip.y };
  textEditor.hidden = false;
  textEditor.textContent = '';
  textEditor.style.left = cssX + 'px';
  textEditor.style.top = cssY + 'px';
  textEditor.style.color = color;
  textEditor.style.fontSize = FONTS[sizeKey] + 'px';
  setTimeout(() => textEditor.focus(), 0);
}

function commitText() {
  if (textEditor.hidden) return;
  const raw = textEditor.innerText.replace(/ /g, ' ');
  const lines = raw.split('\n').map((s) => s.trimEnd()).filter((s, i, arr) => s || i < arr.length - 1);
  if (lines.length && lines.some((s) => s.trim())) {
    pushAnno({ type: 'text', x: textPending.imgX, y: textPending.imgY, lines, color, fontPx: FONTS[sizeKey] });
  }
  textEditor.hidden = true;
  textPending = null;
}

function cancelText() {
  textEditor.hidden = true;
  textPending = null;
}

textEditor.addEventListener('keydown', (e) => {
  e.stopPropagation();
  if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); commitText(); }
  if (e.key === 'Escape') { e.preventDefault(); cancelText(); }
});

/* ============================ toolbar wiring ============================ */
toolbar.addEventListener('mousedown', (e) => e.stopPropagation());
optionsBar.addEventListener('mousedown', (e) => e.stopPropagation());

document.querySelectorAll('#toolButtons .tb-btn').forEach((b) => {
  b.addEventListener('click', () => setTool(b.dataset.tool));
});

function setTool(t) {
  if (!textEditor.hidden) commitText();
  tool = t;
  document.querySelectorAll('#toolButtons .tb-btn').forEach((b) => b.classList.toggle('active', b.dataset.tool === t));
  positionToolbar();
  scheduleRender();
}

document.querySelectorAll('.size-btn').forEach((b) => {
  b.addEventListener('click', () => {
    sizeKey = b.dataset.size;
    document.querySelectorAll('.size-btn').forEach((x) => x.classList.toggle('active', x === b));
    if (!textEditor.hidden) textEditor.style.fontSize = FONTS[sizeKey] + 'px';
  });
});

document.getElementById('btnUndo').addEventListener('click', undo);
document.getElementById('btnRedo').addEventListener('click', redo);
document.getElementById('btnOcr').addEventListener('click', () => enterTextMode());
document.getElementById('btnPin').addEventListener('click', () => doAction('pin'));
document.getElementById('btnRec').addEventListener('click', () => doAction('record'));
document.getElementById('btnSave').addEventListener('click', () => doAction('save'));
document.getElementById('btnClaude').addEventListener('click', () => doAction('claude'));
document.getElementById('btnCopy').addEventListener('click', () => doAction('copy'));
document.getElementById('btnCancel').addEventListener('click', () => doAction('cancel'));

function undo() {
  if (!annotations.length) return;
  const a = annotations.pop();
  redoStack.push(a);
  if (a.type === 'counter') counterN = Math.max(1, counterN - 1);
  scheduleRender(); updateUndoButtons();
}
function redo() {
  if (!redoStack.length) return;
  const a = redoStack.pop();
  annotations.push(a);
  if (a.type === 'counter') counterN = a.n + 1;
  scheduleRender(); updateUndoButtons();
}

/* ============================ keyboard ============================ */
window.addEventListener('keydown', (e) => {
  if (!textEditor.hidden) return; // editor handles its own keys
  const code = e.code; // layout-independent (works on RU keyboard too)

  // Live Text mode owns the keyboard while active
  if (textMode) {
    if (e.key === 'Escape') { e.preventDefault(); if (!translateCard.hidden) { translateCard.hidden = true; } else exitTextMode(); return; }
    if (e.ctrlKey && code === 'KeyA') { e.preventDefault(); selWords = new Set(words.map((_, i) => i)); renderWords(); return; }
    if (e.ctrlKey && code === 'KeyC') { e.preventDefault(); copyTextSelection(false); return; }
    if (e.key === 'Enter') { e.preventDefault(); copyTextSelection(true); return; }
    if (code === 'KeyR') { e.preventDefault(); translateSelection(); return; }
    return;
  }

  if (e.key === 'Alt') {
    e.preventDefault();
    if (!altDown) { altDown = true; updateHover(); }
    return;
  }
  if (e.key === 'Shift' && mode === 'idle') {
    if (!shiftDown) { shiftDown = true; updateHover(); } // hide window highlight → click = whole screen
    return;
  }
  if (e.key === 'Escape') {
    if (drag) { drag = null; tempShape = null; if (mode === 'selecting') { sel = null; mode = 'idle'; } scheduleRender(); return; }
    doAction('cancel');
    return;
  }
  if (e.key === 'Enter' && mode === 'selected') { doAction('copy'); return; }
  if (e.ctrlKey && code === 'KeyC') {
    if (mode === 'selected') doAction('copy');
    return;
  }
  if (e.ctrlKey && code === 'KeyS') { e.preventDefault(); if (mode === 'selected') doAction('save'); return; }
  if (e.ctrlKey && code === 'KeyZ') { e.preventDefault(); e.shiftKey ? redo() : undo(); return; }
  if (e.ctrlKey && code === 'KeyY') { e.preventDefault(); redo(); return; }

  if (mode === 'idle' && code === 'KeyC') {
    // copy pixel color under cursor
    const ix = clamp(Math.round(mouse.x * kx), 0, imgW - 1);
    const iy = clamp(Math.round(mouse.y * ky), 0, imgH - 1);
    const px = srcCtx.getImageData(ix, iy, 1, 1).data;
    const hex = '#' + [px[0], px[1], px[2]].map((v) => v.toString(16).padStart(2, '0')).join('').toUpperCase();
    window.shotik.copyColor(hex);
    window.shotik.cancel();
    return;
  }
  if (code === 'KeyF') {
    sel = { x: 0, y: 0, w: imgW, h: imgH };
    mode = 'selected';
    scheduleRender();
    requestAnimationFrame(positionToolbar);
    return;
  }
  if (mode === 'selected') {
    if (code === 'KeyP') { doAction('pin'); return; }
    if (code === 'KeyV') { doAction('record'); return; }
    if (code === 'KeyT') { enterTextMode(); return; }
    if (code === 'KeyA') { doAction('claude'); return; }
    if (code === 'BracketLeft' || code === 'BracketRight') {
      const order = ['S', 'M', 'L'];
      const i = clamp(order.indexOf(sizeKey) + (code === 'BracketRight' ? 1 : -1), 0, 2);
      sizeKey = order[i];
      positionToolbar();
      return;
    }
    const toolKeys = {
      Digit1: 'move', Digit2: 'pen', Digit3: 'arrow', Digit4: 'line', Digit5: 'rect',
      Digit6: 'ellipse', Digit7: 'highlight', Digit8: 'blur', Digit9: 'text', Digit0: 'counter',
    };
    if (toolKeys[code]) { setTool(toolKeys[code]); return; }
    // arrows nudge selection
    const step = (e.shiftKey ? 10 : 1) * kx;
    let moved = false;
    if (e.key === 'ArrowLeft') { sel.x = clamp(sel.x - step, 0, imgW - sel.w); moved = true; }
    if (e.key === 'ArrowRight') { sel.x = clamp(sel.x + step, 0, imgW - sel.w); moved = true; }
    if (e.key === 'ArrowUp') { sel.y = clamp(sel.y - step, 0, imgH - sel.h); moved = true; }
    if (e.key === 'ArrowDown') { sel.y = clamp(sel.y + step, 0, imgH - sel.h); moved = true; }
    if (moved) { e.preventDefault(); scheduleRender(); positionToolbar(); }
  }
});

/* ============================ actions ============================ */
let actionInFlight = false;

async function doAction(action) {
  if (actionInFlight) return;
  if (action === 'cancel') { window.shotik.cancel(); return; }
  if (!sel || mode !== 'selected') return;
  // recording only needs the region rect (no rendered PNG)
  if (action === 'record') {
    const r = roundSel();
    window.shotik.action({ action: 'record', displayId: DISPLAY_ID, rectPhys: r, rectCss: { x: r.x / kx, y: r.y / ky, w: r.w / kx, h: r.h / ky } });
    return;
  }
  actionInFlight = true;
  try {
    if (!textEditor.hidden) commitText();
    const r = roundSel();
    const oc = document.createElement('canvas');
    oc.width = r.w; oc.height = r.h;
    const octx = oc.getContext('2d');
    octx.drawImage(srcCv, r.x, r.y, r.w, r.h, 0, 0, r.w, r.h);
    for (const a of annotations) drawAnno(octx, a, r.x, r.y);
    const blob = await new Promise((res) => oc.toBlob(res, 'image/png'));
    const buf = new Uint8Array(await blob.arrayBuffer());
    await window.shotik.action({
      action,
      png: buf,
      displayId: DISPLAY_ID,
      rectPhys: r,
      rectCss: { x: r.x / kx, y: r.y / ky, w: r.w / kx, h: r.h / ky },
      forClaude: FOR_CLAUDE,
    });
  } finally {
    actionInFlight = false;
  }
}

window.addEventListener('keyup', (e) => {
  if (e.key === 'Alt') { altDown = false; updateHover(); }
  if (e.key === 'Shift') { shiftDown = false; updateHover(); }
});

/* initial tool */
setTool('move');
