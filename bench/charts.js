'use strict';
// Draws the comparison charts in docs/charts from the raw results in bench/results:
//   node bench/charts.js
//
// Electron is the better of its last two versions on every row, so the comparison is against its best.
const fs = require('fs');
const path = require('path');

const ELECTRON = ['electron-1.3.0', 'electron-1.3.1'];
const RUST = 'rust-2.1.1';
const OUT = path.join(__dirname, '..', 'docs', 'charts');

const read = (label) => JSON.parse(fs.readFileSync(path.join(__dirname, 'results', label + '.json'), 'utf8'));
const results = { electron: ELECTRON.map(read), rust: read(RUST) };
const at = (result, key) => key.split('.').reduce((value, part) => value[part], result);
const electron = (key) => Math.min(...results.electron.map((result) => at(result, key)));
const rust = (key) => at(results.rust, key);

// Slots 1 and 2 of the validated reference palette: `validate_palette.js` passes them on both surfaces.
const THEMES = {
  light: { surface: '#fcfcfb', edge: '#e8e7e3', primary: '#0b0b0b', secondary: '#52514e', muted: '#6f6e69', electron: '#2a78d6', rust: '#eb6834' },
  dark: { surface: '#1a1a19', edge: '#33322e', primary: '#f0efec', secondary: '#c3c2b7', muted: '#9c9b91', electron: '#3987e5', rust: '#d95926' },
};

const TEXT = {
  en: {
    memoryTitle: 'Memory: less is better',
    memorySub: 'Private memory, MB. Electron is the better of 1.3.0 and 1.3.1.',
    memoryNote: 'Benchmark, in ghost mode. On a real desktop, with a tray icon and hotkeys, Rust uses about 6 MB in the tray.',
    speedTitle: 'Speed: less is better',
    speedSub: 'Median time, ms. Electron is the better of 1.3.0 and 1.3.1.',
    speedNote: 'Benchmark, in ghost mode. On a real desktop the Rust overlay appears about 150 ms after the key press.',
    tray: 'In the tray', overlays: 'With overlays open', closed: 'After closing them', video: 'Recording video, peak',
    startup: 'Startup to ready', overlay: 'Overlay shown', ocr: 'OCR of a selection', videoStart: 'Video recording starts',
    mb: 'MB', ms: 'ms', separator: ',',
  },
  ru: {
    memoryTitle: 'Память: меньше — лучше',
    memorySub: 'Закрытая память, МБ. Electron: лучшая из 1.3.0 и 1.3.1.',
    memoryNote: 'Бенчмарк в ghost-режиме. На настоящем столе, со значком в трее и хоткеями, Rust занимает около 6 МБ в трее.',
    speedTitle: 'Скорость: меньше — лучше',
    speedSub: 'Медиана времени, мс. Electron: лучшая из 1.3.0 и 1.3.1.',
    speedNote: 'Бенчмарк в ghost-режиме. На настоящем столе оверлей Rust появляется примерно через 150 мс после нажатия.',
    tray: 'В трее', overlays: 'С открытыми оверлеями', closed: 'После их закрытия', video: 'Запись видео, пик',
    startup: 'Запуск до готовности', overlay: 'Показ оверлея', ocr: 'OCR выделения', videoStart: 'Старт записи видео',
    mb: 'МБ', ms: 'мс', separator: ' ',
  },
};

const CHARTS = {
  memory: {
    title: 'memoryTitle', sub: 'memorySub', note: 'memoryNote', unit: 'mb',
    rows: [
      ['tray', 'idle.treePrivateMB'],
      ['overlays', 'overlay.peakTreePrivateMB'],
      ['closed', 'overlay.treePrivateAfterSettleMB'],
      ['video', 'video.peakTreePrivateMB'],
    ],
  },
  speed: {
    title: 'speedTitle', sub: 'speedSub', note: 'speedNote', unit: 'ms',
    rows: [
      ['startup', 'startup.readyMs.median'],
      ['overlay', 'overlay.warmShowMs.median'],
      ['take_screenshot', 'mcp.takeScreenshotMs.median'],
      ['capture_window', 'mcp.captureWindowMs.median'],
      ['ocr', 'ocr.selectionMs.median'],
      ['videoStart', 'video.startMs'],
    ],
  },
};

const WIDTH = 760;
const LEFT = 232;
const RIGHT = 100;
const BAR = 16;
const GAP = 2;
const ROW = BAR * 2 + GAP + 16;
const TOP = 88;
const FONT = "-apple-system, 'Segoe UI', system-ui, sans-serif";

const escapeXml = (text) => String(text).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
const number = (value, text) => String(Math.round(value)).replace(/\B(?=(\d{3})+$)/g, text.separator);

/** Square at the baseline, a 4 px rounded end, and never more rounding than the bar is long. */
function bar(x, y, length, color) {
  const w = Math.max(length, 2);
  const r = Math.min(4, w, BAR / 2);
  return `<path d="M${x} ${y}h${(w - r).toFixed(1)}a${r} ${r} 0 0 1 ${r} ${r}v${BAR - 2 * r}a${r} ${r} 0 0 1 -${r} ${r}h-${(w - r).toFixed(1)}z" fill="${color}"/>`;
}

function chart(spec, lang, mode) {
  const text = TEXT[lang];
  const theme = THEMES[mode];
  const rows = spec.rows.map(([name, key]) => ({ label: text[name] || name, electron: electron(key), rust: rust(key) }));
  const max = Math.max(...rows.flatMap((row) => [row.electron, row.rust]));
  const scale = (WIDTH - LEFT - RIGHT) / max;
  const unit = text[spec.unit];
  const height = TOP + rows.length * ROW + 34;
  const words = (size, color, extra = '') => `font-size="${size}" fill="${color}" ${extra}`;
  const label = (x, y, value, size, color, extra) => `<text x="${x}" y="${y}" ${words(size, color, extra)}>${escapeXml(value)}</text>`;

  const parts = [
    `<rect x="0.5" y="0.5" width="${WIDTH - 1}" height="${height - 1}" rx="10" fill="${theme.surface}" stroke="${theme.edge}"/>`,
    label(20, 32, text[spec.title], 16, theme.primary, 'font-weight="600"'),
    label(20, 52, text[spec.sub], 12, theme.secondary),
  ];
  // The legend: identity is never color alone, and every bar below repeats its value.
  parts.push(`<rect x="${LEFT}" y="64" width="10" height="10" rx="2" fill="${theme.electron}"/>`);
  parts.push(label(LEFT + 15, 73, 'Electron', 12, theme.secondary));
  parts.push(`<rect x="${LEFT + 84}" y="64" width="10" height="10" rx="2" fill="${theme.rust}"/>`);
  parts.push(label(LEFT + 99, 73, 'Rust', 12, theme.secondary));

  rows.forEach((row, index) => {
    const y = TOP + index * ROW;
    parts.push(label(20, y + BAR + GAP / 2 + 4, row.label, 13, theme.primary));
    parts.push(bar(LEFT, y, row.electron * scale, theme.electron));
    parts.push(bar(LEFT, y + BAR + GAP, row.rust * scale, theme.rust));
    parts.push(label(LEFT + Math.max(row.electron * scale, 2) + 8, y + BAR - 4, `${number(row.electron, text)} ${unit}`, 12, theme.secondary));
    parts.push(label(LEFT + Math.max(row.rust * scale, 2) + 8, y + BAR * 2 + GAP - 4, `${number(row.rust, text)} ${unit}`, 12, theme.primary, 'font-weight="600"'));
  });
  parts.push(`<line x1="${LEFT}" y1="${TOP - 4}" x2="${LEFT}" y2="${TOP + rows.length * ROW - 12}" stroke="${theme.edge}"/>`);
  parts.push(label(20, height - 14, text[spec.note], 11, theme.muted));

  const summary = rows.map((row) => `${row.label}: Electron ${number(row.electron, text)} ${unit}, Rust ${number(row.rust, text)} ${unit}`).join('; ');
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${WIDTH}" height="${height}" viewBox="0 0 ${WIDTH} ${height}" role="img" aria-labelledby="t d" font-family="${FONT}">
<title id="t">${escapeXml(text[spec.title])}</title>
<desc id="d">${escapeXml(summary)}</desc>
${parts.join('\n')}
</svg>
`;
}

fs.mkdirSync(OUT, { recursive: true });
for (const [name, spec] of Object.entries(CHARTS)) {
  for (const lang of Object.keys(TEXT)) {
    for (const mode of Object.keys(THEMES)) {
      const file = path.join(OUT, `${name}-${lang}-${mode}.svg`);
      fs.writeFileSync(file, chart(spec, lang, mode));
      console.log(path.relative(process.cwd(), file));
    }
  }
}
