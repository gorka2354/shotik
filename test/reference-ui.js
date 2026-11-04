'use strict';
// Rebuild the Electron visual specification. Every pixel comes from the
// product's off-screen renderer and test/fake-screen.png, never the desktop.
const fs = require('fs');
const net = require('net');
const path = require('path');
const { execFileSync } = require('child_process');
const { PNG } = require('pngjs');
const { makeApp } = require('./ghost/adapter');

const ROOT = path.join(__dirname, '..');
const OUT = path.join(ROOT, 'docs', 'rust', 'reference-ui');
const FIXTURE = path.join(__dirname, 'fake-screen.png');

async function freePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => {
    server.once('error', reject); server.listen(0, '127.0.0.1', resolve);
  });
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

async function until(app, check, label) {
  for (let i = 0; i < 100; i++) {
    const result = await check();
    if (result) return result;
    await app.sleep(100);
  }
  throw new Error('Reference state did not settle: ' + label);
}

function seedHistory(app) {
  const source = PNG.sync.read(fs.readFileSync(FIXTURE));
  const crops = [
    { x: 240, y: 140, w: 1100, h: 640 },
    { x: 270, y: 225, w: 700, h: 190 },
    { x: 1440, y: 150, w: 240, h: 100 },
  ];
  const entries = crops.map((r, i) => {
    const image = new PNG({ width: r.w, height: r.h });
    PNG.bitblt(source, image, r.x, r.y, r.w, r.h, 0, 0);
    const file = path.join(app.saveDir, `Reference-${i + 1}.png`);
    fs.writeFileSync(file, PNG.sync.write(image));
    return { id: `fixture-${i + 1}`, file, thumb: file, width: r.w, height: r.h,
      ts: Date.now() - (i * 12 + 2) * 60000, source: ['region', 'fullscreen', 'repeat'][i], kind: 'image' };
  });
  fs.writeFileSync(path.join(app.profile, 'history.json'), JSON.stringify(entries));
  return entries;
}

async function main() {
  const port = await freePort();
  const app = makeApp({ port, impl: 'electron', env: { SHOTIK_FORCE_CONTROLS: '1', SHOTIK_TP_TTL: '60000' },
    settings: {
      language: 'en', showToast: true, mcp: { enabled: true, port },
      translate: { target: 'ru', provider: 'free', deeplKey: '' },
      recording: { countdown: false, systemAudio: true, microphone: true },
      audioRecording: { systemAudio: true, microphone: true },
      // An intentionally unknown display id follows the reference's primary
      // fallback; both the remembered crop and its source are synthetic.
      lastRegion: { displayId: 0, rectPhys: { x: 240, y: 140, width: 1100, height: 640 } },
    } });
  const entries = seedHistory(app);
  const images = [];
  fs.mkdirSync(OUT, { recursive: true });

  async function capture(name, target, description, index) {
    // Let fonts, image decode, layout and the compositor present the state.
    await app.sleep(180);
    const file = path.join(OUT, name + '.png');
    let result;
    // A just-shown surface can acknowledge capture before its first compositor
    // frame. Require a real nonempty PNG, retrying only that initial empty state.
    for (let attempt = 0; attempt < 10; attempt++) {
      result = await app.capturePage(target, file, index);
      if (result.size.width > 0 && result.size.height > 0 && fs.statSync(file).size > 0) break;
      await app.sleep(200);
    }
    if (!fs.statSync(file).size) throw new Error('Surface never painted: ' + name);
    const png = PNG.sync.read(fs.readFileSync(file));
    if (png.width < 10 || png.height < 10) throw new Error('Empty capture: ' + name);
    let painted = false;
    for (let i = 3; i < png.data.length; i += 4) if (png.data[i]) { painted = true; break; }
    if (!painted) throw new Error('Transparent capture: ' + name);
    images.push({ name, description, width: png.width, height: png.height });
    console.log(`${name}.png ${result.size.width}x${result.size.height}`);
  }

  async function finishedRecording() {
    await app.post('record-stop');
    await until(app, async () => !(await app.post('record-status')).recording, 'record stopped');
    await until(app, async () => (await app.post('state')).lastProcessed?.action === 'record-done', 'record saved');
  }

  try {
    await app.waitReady();
    await until(app, async () => (await app.uiState('main')).ready, 'main ready');
    for (const theme of ['light', 'dark']) {
      await app.post('set-theme', { mode: theme });
      for (const [page, name] of [['history', 'shots'], ['text', 'text'], ['claude', 'claude'], ['settings', 'settings']]) {
        if (page === 'text') {
          await app.uiAction('main', { type: 'quick-translate', text: 'Hello Shotik, this is a reference translation.' });
          await until(app, async () => {
            const s = (await app.uiState('main')).text;
            return s.resultVisible && !s.loading && s.result.includes('Hello Shotik');
          }, 'quick translation');
        } else await app.uiAction('main', { type: 'navigate', page });
        await until(app, async () => (await app.uiState('main')).page === page, 'main ' + page);
        await capture(`main-${name}-${theme}`, 'main', `${name} page, ${theme} theme${page === 'history' ? ', three cropped fake screenshots' : page === 'text' ? ', fake input and stub translation' : ''}.`);
      }
    }

    await app.post('trigger', { mode: 'region' });
    const state = await app.post('state');
    const index = state.overlayBounds.findIndex((b) => b.x === 20000 && b.y === 0);
    if (index < 0) throw new Error('No ghost overlay for the primary display');
    const overlay = await until(app, async () => { const s = await app.uiState('overlay', index); return s.ready && s; }, 'overlay frame');
    if (!overlay.hintsVisible) throw new Error('Idle overlay hints are not visible');
    await capture('overlay-idle', 'overlay', 'Idle overlay over fake-screen.png, with hint chips.', index);
    const sx = (n) => n * overlay.viewport.width / 1920;
    const sy = (n) => n * overlay.viewport.height / 1080;
    await app.uiAction('overlay', { type: 'select', rect: { x: sx(240), y: sy(140), w: sx(1100), h: sy(640) } }, index);
    if (!(await app.uiState('overlay', index)).toolbarVisible) throw new Error('Selection toolbar is hidden');
    await capture('overlay-selection', 'overlay', 'Fake window selected, resize handles, dimensions and action/tool toolbar.', index);
    await app.uiAction('overlay', { type: 'annotate', annotations: [
      { type: 'rect', x1: sx(270), y1: sy(225), x2: sx(940), y2: sy(330), color: '#FF4D5E', stroke: 4 },
      { type: 'arrow', x1: sx(1100), y1: sy(460), x2: sx(940), y2: sy(305), color: '#3E9EFF', stroke: 4 },
      { type: 'counter', x: sx(270), y: sy(210), n: 1, color: '#FF9F2E' },
      { type: 'text', x: sx(340), y: sy(450), lines: ['Review this area'], fontPx: 23, color: '#FFFFFF' },
    ] }, index);
    if ((await app.uiState('overlay', index)).annotationCount !== 4) throw new Error('Missing reference annotations');
    await capture('overlay-annotations', 'overlay', 'Same selection with rectangle, arrow, numbered marker and text annotation.', index);
    await app.uiAction('overlay', { type: 'annotate', annotations: [] }, index);
    await app.uiAction('overlay', { type: 'live-text' }, index);
    await until(app, async () => { const s = await app.uiState('overlay', index); return s.textMode && s.wordCount > 0; }, 'Live Text OCR boxes');
    await app.uiAction('overlay', { type: 'text-select-all' }, index);
    await app.sleep(1500); // let the reference's 1400ms "Recognizing text" flash expire
    await capture('overlay-live-text', 'overlay', 'Live Text: real OCR of fake EN/RU fixture text, all word boxes selected and text toolbar.', index);
    await app.uiAction('overlay', { type: 'copy-color', hex: '#3E9EFF' }, index);
    await until(app, async () => (await app.post('last-toast'))?.kind === 'color', 'color toast');
    await capture('toast-color', 'toast', 'Color copied: actual blue color swatch and hexadecimal value.');
    await app.cancelOverlay();
    await until(app, async () => !(await app.post('state')).overlayOpen, 'overlay canceled');

    const pngBase64 = fs.readFileSync(FIXTURE).toString('base64');
    await app.post('sim-action', { action: 'copy', pngBase64 });
    await capture('toast-image', 'toast', 'Image copied, with thumbnail made from fake-screen.png.');
    await app.post('sim-action', { action: 'claude', pngBase64 });
    await capture('toast-claude', 'toast', 'Copy for Claude completed, with fake image thumbnail.');
    await app.post('trigger', { mode: 'repeat' });
    await capture('toast-repeat', 'toast', 'Repeat capture completed, with thumbnail of the fake remembered region.');
    await app.post('show-toast', { kind: 'text', title: 'Text copied', body: 'Hello Shotik 123 — fixture text only.' });
    await capture('toast-text', 'toast', 'Text notification with fixture-only title and body.');
    await app.post('show-toast', { kind: 'gif', title: 'GIF ready', body: 'Reference-clip.gif' });
    await capture('toast-gif', 'toast', 'Configured gif notification kind, default checkmark icon and fake GIF name.');

    await app.uiAction('main', { type: 'history-pin', id: entries[0].id });
    await until(app, async () => (await app.uiState('pin')).ready, 'pin image');
    await capture('pin', 'pin', 'Pinned cropped fake window in idle state; hover controls are hidden.');
    const bubble = await app.post('selection-bubble', { text: 'Hello Shotik, please review this example.', x: 900, y: 500 });
    if (!bubble.bubble || bubble.bounds.x < 20000) throw new Error('Selection bubble is not a ghost window');
    await capture('selection-bubble', 'bubble', 'Selection translation bubble raised for fake text.');
    await app.post('bubble-clear');
    await app.post('show-tp-loading', { text: 'Hello Shotik, please review this example.' });
    if (!(await app.uiState('tp')).loading) throw new Error('Translate loading state missing');
    await capture('translate-loading', 'tp', 'Translate popup loading, original fixture text and loading indicator.');
    await app.post('resolve-tp', { text: 'Hello Shotik, please review this example.', translated: 'Привет, Shotik, проверь этот пример.' });
    if ((await app.uiState('tp')).loading) throw new Error('Translate result is still loading');
    await capture('translate-result', 'tp', 'Translate popup with explicit fake EN→RU result, language pair and Copy control.');

    await app.recordStart();
    await until(app, async () => {
      const s = await app.post('record-status'); return s.audio && s.levels;
    }, 'synthetic recorder');
    await until(app, async () => {
      const s = await app.uiState('rec-controls');
      return s.meters.length === 2 && s.meters.every((m) => m.fill > 0.2 * m.track);
    }, 'controls meters');
    await capture('recording-controls', 'rec-controls', 'Active screen recording controls: timer, synthetic system/microphone meters, pause, stop, cancel.');
    await capture('recording-border', 'rec-border', 'Transparent 3px recording border around the fake default region.');
    await finishedRecording();
    await capture('toast-video', 'toast', 'Finished synthetic screen recording notification.');
    await app.post('trigger', { mode: 'audio' });
    await until(app, async () => (await app.post('record-status')).audio, 'synthetic audio recorder');
    await app.sleep(1500);
    await finishedRecording();
    await capture('toast-audio', 'toast', 'Finished synthetic audio-only recording notification.');

    const commit = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: ROOT, encoding: 'utf8', windowsHide: true }).trim();
    const readme = [
      '# Electron UI reference', '',
      'Regenerate from the repository root: `node test/reference-ui.js`.', '',
      `Reference source: Electron v${require('../package.json').version}, commit \`${commit}\`.`, '',
      'All PNGs are captured in-process through the ghost capture-page route. The generator',
      'uses a fresh temporary profile, fixture-only settings/history, test/fake-screen.png',
      'and synthetic recording sources. It restores the clipboard and removes the profile',
      'afterwards. It does not read the real history/settings or capture desktop pixels/audio.', '',
      'The main pages are shown in both native themes. Small windows keep their actual',
      'reference styling (some remain dark regardless of the main theme). PNG sizes reflect',
      'the renderer scale of the monitor layout; compare layout in DIP, not PNG dimensions.',
      'The GIF export completion currently uses the video presentation in the product;',
      'toast-gif additionally records the explicitly configured gif kind accepted by showToast.', '',
      '| PNG | Pixel size | State |', '|---|---|---|',
      ...images.map((i) => `| [${i.name}.png](${i.name}.png) | ${i.width}×${i.height} | ${i.description} |`), '',
      'Coverage: 8 main-page/theme images, 4 overlay/Live Text images, 8 toast kinds,',
      'pin, selection bubble, 2 translation states, recording controls and border (26 PNGs).', '',
    ].join('\n');
    fs.writeFileSync(path.join(OUT, 'README.md'), readme, 'utf8');
    if (images.length !== 26) throw new Error('Incomplete reference image set: ' + images.length);
    console.log(`Wrote ${images.length} reference PNGs and README.md`);
  } finally { await app.stop(); }
}

main().catch((e) => { console.error(e.stack || e); process.exitCode = 1; });
