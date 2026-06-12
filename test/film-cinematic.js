'use strict';
/* Films smooth (12 fps) demo footage by driving a ghost-mode Shotik instance.
   Every small cursor step is captured as a frame; a manifest records cursor
   position + camera zoom hint per frame for the virtual-camera renderer.
     node test/film-cinematic.js <framesDir>
   then: node test/render-camera.js <framesDir> docs/demo.gif */

const fs = require('fs');
const path = require('path');

const BASE = 'http://127.0.0.1:' + (process.env.SHOTIK_PORT || 7465);
const OUT = process.argv[2] || path.join(__dirname, 'frames-cine');

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const easeInOut = (t) => (t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2);

async function post(route, body = {}) {
  const res = await fetch(`${BASE}/test/${route}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  const j = await res.json();
  if (!j.ok) throw new Error(`${route}: ${j.error}`);
  return j.result;
}
const ev = (events) => post('input', { events });
const js = (code) => post('exec-js', { code });

let curX = 1400, curY = 900;
async function cursorTo(x, y) {
  curX = x; curY = y;
  await js(`(() => {
    let c = document.getElementById('fakeCur');
    if (!c) {
      c = document.createElement('div');
      c.id = 'fakeCur';
      c.style.cssText = 'position:fixed;z-index:99999;pointer-events:none;width:24px;height:24px;filter:drop-shadow(0 1px 3px rgba(0,0,0,.65))';
      c.innerHTML = '<svg viewBox="0 0 24 24" width="24" height="24"><path d="M5 3l14 7-6.5 1.5L9 18 5 3z" fill="#fff" stroke="#1b1b1b" stroke-width="1.3"/></svg>';
      document.body.appendChild(c);
    }
    c.style.left = '${x}px'; c.style.top = '${y}px';
    return 1;
  })()`);
}

const manifest = [];
let frameN = 0;
let sceneZoom = 1;

async function frame(hold = 0) {
  frameN++;
  const file = `f${String(frameN).padStart(4, '0')}.png`;
  await post('capture-page', { path: (OUT + '/' + file).replace(/\\/g, '/') });
  manifest.push({ file, x: curX, y: curY, zoom: sceneZoom, hold });
  if (frameN % 20 === 0) console.log('frame', frameN);
}

// glide the cursor with easing, capturing every step
async function glide(x2, y2, steps, { drag = false } = {}) {
  const x1 = curX, y1 = curY;
  for (let i = 1; i <= steps; i++) {
    const t = easeInOut(i / steps);
    const x = Math.round(x1 + (x2 - x1) * t);
    const y = Math.round(y1 + (y2 - y1) * t);
    await ev([{ type: 'mouseMove', x, y }]);
    await cursorTo(x, y);
    await frame();
  }
  if (drag) return;
}
async function down() { await ev([{ type: 'mouseDown', x: curX, y: curY, button: 'left', clickCount: 1 }]); }
async function up() { await ev([{ type: 'mouseUp', x: curX, y: curY, button: 'left', clickCount: 1 }]); }
async function clickHere(holdAfter = 0) {
  await down(); await up();
  await frame(holdAfter);
}
async function hold(frames, holdMs = 0) {
  for (let i = 0; i < frames; i++) await frame(i === frames - 1 ? holdMs : 0);
}

(async () => {
  fs.mkdirSync(OUT, { recursive: true });
  await post('set-theme', { mode: 'dark' });
  await post('trigger', { mode: 'region' });
  await sleep(1100);

  // Scene 1: wide — drift towards the window, hover-snap pops
  sceneZoom = 1;
  await cursorTo(1480, 950);
  await hold(4);
  await glide(720, 420, 16);
  await hold(6, 350);

  // Scene 2: click — window selected, toolbar appears; camera pushes in
  sceneZoom = 1.5;
  await clickHere(350);
  await hold(6);

  const btns = JSON.parse(await js(
    `JSON.stringify(Object.fromEntries([...document.querySelectorAll('#toolbar .tb-btn')].map(b => { const r = b.getBoundingClientRect(); return [b.dataset.tool || b.id, [Math.round(r.x + r.width/2), Math.round(r.y + r.height/2)]]; })))`));

  // Scene 3: arrow tool, draw an arrow pointing at the title
  await glide(btns.arrow[0], btns.arrow[1], 10);
  await clickHere(250);
  sceneZoom = 1.45;
  await glide(1150, 800, 8);
  await down();
  await glide(660, 305, 14, { drag: true });
  await up();
  await frame(400);
  await hold(3);

  // Scene 4: frame around the headline
  await glide(btns.rect[0], btns.rect[1], 8);
  await clickHere(200);
  sceneZoom = 1.65;
  await glide(355, 285, 8);
  await down();
  await glide(672, 357, 10, { drag: true });
  await up();
  await frame(400);
  await hold(3);

  // Scene 5: pixelate the secret line
  await glide(btns.blur[0], btns.blur[1], 8);
  await clickHere(200);
  sceneZoom = 1.65;
  await glide(372, 452, 7);
  await down();
  await glide(830, 500, 10, { drag: true });
  await up();
  await frame(450);
  await hold(3);

  // Scene 6: numbered markers
  await glide(btns.counter[0], btns.counter[1], 8);
  await clickHere(200);
  sceneZoom = 1.5;
  await glide(352, 256, 7);
  await clickHere(250);
  await glide(845, 474, 7);
  await clickHere(300);

  // Scene 7: typed note, char by char
  await glide(btns.text[0], btns.text[1], 8);
  await clickHere(200);
  sceneZoom = 1.6;
  await glide(880, 295, 7);
  await clickHere(150);
  await sleep(200);
  const note = 'Fix me!';
  for (let i = 1; i <= note.length; i++) {
    await js(`document.getElementById('textEditor').innerText = ${JSON.stringify(note.slice(0, i))}; 1`);
    await frame(i === note.length ? 350 : 60);
  }
  await js('commitText(); 1');
  await frame(350);

  // Scene 8: pull back wide, then copy
  sceneZoom = 1;
  await glide(btns.btnCopy[0], btns.btnCopy[1], 14);
  await hold(5, 300);
  await down(); await up();
  await sleep(900);
  await post('capture-page', { target: 'toast', path: (OUT + '/toast.png').replace(/\\/g, '/') });

  fs.writeFileSync(path.join(OUT, 'manifest.json'), JSON.stringify(manifest));
  console.log('done:', frameN, 'frames');
})().catch((e) => { console.error(e); process.exit(1); });
