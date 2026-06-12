'use strict';
/* Films the README demo GIF frames by driving a ghost-mode Shotik instance
   through its /test endpoints. Run the app first:
     SHOTIK_TEST=1 SHOTIK_GHOST=1 SHOTIK_PORT=7465 SHOTIK_FAKE_SCREEN=test/fake-screen.png npm start -- --hidden
   then: node test/film-demo.js <framesDir> */

const fs = require('fs');
const path = require('path');

const BASE = 'http://127.0.0.1:' + (process.env.SHOTIK_PORT || 7465);
const OUT = process.argv[2] || path.join(__dirname, 'frames');

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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

// fake cursor that survives re-renders (DOM overlay, not canvas)
async function cur(x, y) {
  await js(`(() => {
    let c = document.getElementById('fakeCur');
    if (!c) {
      c = document.createElement('div');
      c.id = 'fakeCur';
      c.style.cssText = 'position:fixed;z-index:99999;pointer-events:none;width:22px;height:22px;filter:drop-shadow(0 1px 2px rgba(0,0,0,.6))';
      c.innerHTML = '<svg viewBox="0 0 24 24" width="22" height="22"><path d="M5 3l14 7-6.5 1.5L9 18 5 3z" fill="#fff" stroke="#1b1b1b" stroke-width="1.3"/></svg>';
      document.body.appendChild(c);
    }
    c.style.left = '${x}px'; c.style.top = '${y}px';
    return 1;
  })()`);
}

let frameN = 0;
async function frame(pause = 150) {
  await sleep(pause);
  frameN++;
  const file = path.join(OUT, `f${String(frameN).padStart(2, '0')}.png`).replace(/\\/g, '/');
  await post('capture-page', { path: file });
  console.log('frame', frameN);
}

async function move(x, y) {
  await ev([{ type: 'mouseMove', x, y }, { type: 'mouseMove', x: x + 1, y }]);
  await cur(x, y);
}
async function click(x, y) {
  await move(x, y);
  await ev([
    { type: 'mouseDown', x, y, button: 'left', clickCount: 1 },
    { type: 'mouseUp', x, y, button: 'left', clickCount: 1 },
  ]);
}
async function drag(x1, y1, x2, y2, midFrame = false) {
  await move(x1, y1);
  await ev([{ type: 'mouseDown', x: x1, y: y1, button: 'left', clickCount: 1 }]);
  const mx = Math.round((x1 + x2) / 2), my = Math.round((y1 + y2) / 2);
  await ev([{ type: 'mouseMove', x: mx, y: my }]);
  await cur(mx, my);
  if (midFrame) await frame();
  await ev([{ type: 'mouseMove', x: x2, y: y2 }, { type: 'mouseUp', x: x2, y: y2, button: 'left', clickCount: 1 }]);
  await cur(x2, y2);
}
// ghost windows are never focused, so synthetic char events don't reach the
// contenteditable editor — set the text and commit through the page API instead
async function typeText(s) {
  await js(`document.getElementById('textEditor').innerText = ${JSON.stringify(s)}; commitText(); 1`);
}

(async () => {
  fs.mkdirSync(OUT, { recursive: true });
  await post('set-theme', { mode: 'dark' });
  await post('trigger', { mode: 'region' });
  await sleep(1000); // freeze + window list

  // 1: idle — hints + magnifier
  await move(1150, 660);
  await frame(250);

  // 2: hover-snap highlights the window under the cursor
  await move(700, 400);
  await frame(250);

  // 3: click — window becomes the selection, toolbar appears
  await click(700, 400);
  await frame(350);

  // toolbar button coordinates
  const btns = JSON.parse(await js(
    `JSON.stringify(Object.fromEntries([...document.querySelectorAll('#toolbar .tb-btn')].map(b => { const r = b.getBoundingClientRect(); return [b.dataset.tool || b.id, [Math.round(r.x + r.width/2), Math.round(r.y + r.height/2)]]; })))`));

  // 4-5: arrow pointing at the title
  await click(...btns.arrow);
  await frame(200);
  await drag(1250, 720, 700, 315, true);
  await frame(250);

  // 6: frame around "Hello Shotik 123"
  await click(...btns.rect);
  await drag(360, 290, 665, 350);
  await frame(250);

  // 7: pixelate the code line
  await click(...btns.blur);
  await drag(373, 452, 825, 497);
  await frame(250);

  // 8: numbered markers
  await click(...btns.counter);
  await click(352, 255);
  await click(845, 472);
  await frame(250);

  // 9: text note
  await click(...btns.text);
  await click(885, 295);
  await sleep(250);
  await typeText('Fix me!');
  await move(1350, 620);
  await frame(300);

  // 10: copy → overlay closes, toast pops
  await click(...btns.btnCopy);
  await sleep(900);
  await post('capture-page', { target: 'toast', path: (OUT + '/toast.png').replace(/\\/g, '/') });
  console.log('toast captured');

  console.log('done:', frameN, 'frames in', OUT);
})().catch((e) => { console.error(e); process.exit(1); });
