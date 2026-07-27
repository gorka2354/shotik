'use strict';
// Ghost tests for the MCP tool fixes: virtual-desktop region coords, argument
// validation (the NaN -> empty PNG bug), display metadata in take_screenshot,
// list_windows / capture_window on the app's OWN off-screen window, the
// hotkey suspend flag and the longer video toast. No real desktop involved.
// Every test gets its OWN port — a dying previous instance must never race a
// fresh one for the listener (that shows up as socket hang up / ghost toasts).
const http = require('http');
const path = require('path');
const { spawn } = require('child_process');
const { test, assert, eq } = require('../tiny');
const { makeApp } = require('./harness');

let nextPort = 7520;

// raw MCP JSON-RPC call to the ghost instance
function mcpCall(port, name, args) {
  return new Promise((resolve, reject) => {
    const data = Buffer.from(JSON.stringify({
      jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args || {} },
    }));
    const req = http.request({ host: '127.0.0.1', port, path: '/mcp', method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Content-Length': data.length } },
      (res) => { let b = ''; res.on('data', (c) => (b += c)); res.on('end', () => {
        try { resolve(JSON.parse(b).result); } catch (e) { reject(new Error('bad rpc reply: ' + b.slice(0, 200))); } }); });
    req.on('error', reject); req.write(data); req.end();
  });
}

const imgOf = (res) => (res.content || []).find((c) => c.type === 'image');
const textOf = (res) => ((res.content || []).find((c) => c.type === 'text') || {}).text || '';

test('mcp: region tool takes GLOBAL virtual-desktop coords on any display', async () => {
  const port = nextPort++;
  const app = makeApp({ port });
  try {
    await app.waitReady();
    const st = await app.post('state', {});
    // prefer a non-primary display when the machine has one — that's the bug case
    const d = st.displays.find((x) => x.bounds.x !== 0 || x.bounds.y !== 0) || st.displays[0];
    const res = await mcpCall(port, 'take_screenshot_region',
      { x: d.bounds.x + 10, y: d.bounds.y + 10, width: 200, height: 120 });
    assert(!res.isError, 'not an error: ' + textOf(res));
    const img = imgOf(res);
    assert(img && img.data && img.data.length > 1000, 'has a real image');
    assert(/Captured 200x120/.test(textOf(res)), 'reports what was captured: ' + textOf(res));
  } finally { await app.stop(); }
});

test('mcp: malformed region args produce a clear error, never an empty image', async () => {
  const port = nextPort++;
  const app = makeApp({ port });
  try {
    await app.waitReady();
    // the historical repro: nested object instead of flat numbers
    const res = await mcpCall(port, 'take_screenshot_region', { region: { x: 0, y: 0, width: 100, height: 100 } });
    assert(res.isError, 'is an error');
    assert(/"x" must be a finite number/.test(textOf(res)), textOf(res));
    assert(!imgOf(res), 'no image payload on error');
    // far outside every display -> explanatory error listing displays
    const res2 = await mcpCall(port, 'take_screenshot_region', { x: 99999, y: 99999, width: 50, height: 50 });
    assert(res2.isError && /does not intersect/.test(textOf(res2)), textOf(res2));
  } finally { await app.stop(); }
});

test('mcp: take_screenshot reports WHICH display it captured', async () => {
  const port = nextPort++;
  const app = makeApp({ port });
  try {
    await app.waitReady();
    // keep test files out of the user's real screenshots folder
    const tmp = path.join(require('os').tmpdir(), 'shotik-ghost-shots').replace(/\\/g, '\\\\');
    await app.post('exec-js', { target: 'main', code: `window.shotik.setSettings({ saveDir: '${tmp}' })` });
    const st = await app.post('state', {});
    const d = st.displays[st.displays.length - 1];
    const res = await mcpCall(port, 'take_screenshot', { display_id: d.id });
    assert(!res.isError, textOf(res));
    assert(new RegExp('Captured display ' + d.id).test(textOf(res)), textOf(res));
  } finally { await app.stop(); }
});

test('mcp: list_windows + capture_window see and shoot an occluded/off-screen window', async () => {
  const port = nextPort++;
  const app = makeApp({ port });
  try {
    await app.waitReady();
    await app.post('show-main', {});
    // unique title so the test can never collide with the user's own windows
    await app.post('exec-js', { target: 'main', code: 'document.title = "ShotikGhostTarget"; true' });
    await app.sleep(600);
    const lw = await mcpCall(port, 'list_windows', {});
    assert(!lw.isError, textOf(lw));
    const items = JSON.parse(textOf(lw));
    const mine = items.find((w) => w.title === 'ShotikGhostTarget');
    assert(mine, 'ghost main window is enumerated: ' + items.map((w) => w.title).slice(0, 10).join(' | '));
    assert(mine.bounds && mine.bounds.width > 100, 'has DIP bounds');

    const cap = await mcpCall(port, 'capture_window', { window_id: mine.id });
    assert(!cap.isError, textOf(cap));
    const img = imgOf(cap);
    assert(img && img.data && img.data.length > 5000, 'captured a real image of the window');
    assert(/Captured window "ShotikGhostTarget"/.test(textOf(cap)), textOf(cap));

    // title-substring path too (case-insensitive)
    const cap2 = await mcpCall(port, 'capture_window', { title: 'shotikghost' });
    assert(!cap2.isError, textOf(cap2));
    assert(imgOf(cap2), 'title match captured');
  } finally { await app.stop(); }
});

test('mcp: capture_window with a bogus target explains itself', async () => {
  const port = nextPort++;
  const app = makeApp({ port });
  try {
    await app.waitReady();
    const res = await mcpCall(port, 'capture_window', { title: 'no-such-window-xyzzy-42' });
    assert(res.isError && /No window matches title/.test(textOf(res)), textOf(res));
    const res2 = await mcpCall(port, 'capture_window', {});
    assert(res2.isError && /window_id .*or title/.test(textOf(res2)), textOf(res2));
  } finally { await app.stop(); }
});

test('mcp: capture_window falls back to desktopCapturer when PrintWindow dies', async () => {
  // Chromium's window capturer EXCLUDES the app's own windows, so the fallback
  // needs an external target: an off-screen WinForms window we spawn ourselves.
  const port = nextPort++;
  const app = makeApp({ port, env: { SHOTIK_WINCAP_FAIL: '1' } });
  const title = 'ShotikCapTest-' + Date.now();
  const win = spawn('powershell.exe',
    ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', path.join(__dirname, 'testwin.ps1'), '-Title', title],
    { windowsHide: true });
  try {
    await app.waitReady();
    // wait until the external window is enumerable
    let seen = null;
    for (let i = 0; i < 20 && !seen; i++) {
      await app.sleep(500);
      const lw = await mcpCall(port, 'list_windows', {});
      if (!lw.isError) seen = JSON.parse(textOf(lw)).find((w) => w.title === title);
    }
    assert(seen, 'external test window appears in list_windows');
    const cap = await mcpCall(port, 'capture_window', { title });
    assert(!cap.isError, textOf(cap));
    assert(/via desktopCapturer/.test(textOf(cap)), 'used the fallback: ' + textOf(cap));
    assert(imgOf(cap) && imgOf(cap).data.length > 1000, 'fallback produced a real image');
  } finally {
    try { win.kill(); } catch (_) {}
    await app.stop();
  }
});

test('hotkeys: focusing a recorder field suspends shortcuts end-to-end', async () => {
  const port = nextPort++;
  const app = makeApp({ port });
  try {
    await app.waitReady();
    await app.post('show-main', {});
    await app.sleep(400);
    // the settings page must be the active tab — a display:none input can't take focus
    await app.post('exec-js', { target: 'main', code: 'document.querySelector(".nav-item[data-page=\'settings\']").click(); document.querySelector("#hkRegion").focus(); document.activeElement.id' });
    await app.sleep(250);
    eq((await app.post('state', {})).hotkeysSuspended, true, 'field focus suspends');
    await app.post('exec-js', { target: 'main', code: 'document.querySelector("#hkRegion").blur(); true' });
    await app.sleep(250);
    eq((await app.post('state', {})).hotkeysSuspended, false, 'blur resumes');
  } finally { await app.stop(); }
});

test('hotkeys: suspend flag toggles and resume re-registers', async () => {
  const port = nextPort++;
  const app = makeApp({ port });
  try {
    await app.waitReady();
    eq((await app.post('hotkeys', { op: 'suspend' })).suspended, true);
    eq((await app.post('state', {})).hotkeysSuspended, true);
    eq((await app.post('hotkeys', { op: 'resume' })).suspended, false);
  } finally { await app.stop(); }
});

test('clipboard: plain Copy is image-only, "copy for Claude" is image+path', async () => {
  const port = nextPort++;
  const app = makeApp({ port });
  try {
    await app.waitReady();
    const tmp = path.join(require('os').tmpdir(), 'shotik-ghost-shots').replace(/\\/g, '\\\\');
    await app.post('exec-js', { target: 'main', code: `window.shotik.setSettings({ saveDir: '${tmp}' })` });
    const pngBase64 = require('fs').readFileSync(path.join(__dirname, '..', 'fake-screen.png')).toString('base64');

    const plain = await app.post('sim-action', { action: 'copy', pngBase64 });
    eq(plain.hasImage, true, 'copy puts the image');
    eq(plain.text, '', 'copy does NOT put the file path: got "' + plain.text + '"');

    const claude = await app.post('sim-action', { action: 'claude', pngBase64 });
    eq(claude.hasImage, true, 'claude puts the image');
    assert(/\.png$/i.test(claude.text), 'claude also puts the path: got "' + claude.text + '"');
  } finally { await app.stop(); }
});

test('toast: a finished recording toast outlives the old 4.2s timeout', async () => {
  const port = nextPort++;
  const app = makeApp({ port });
  try {
    await app.waitReady();
    const shown = await app.post('show-toast', { kind: 'video' });
    eq(shown.visible, true, 'toast shown');
    await app.sleep(5000); // > old 4200ms TTL
    eq((await app.post('toast-visible', {})).visible, true, 'video toast still up after 5s');
  } finally { await app.stop(); }
});

test('toast: hover pause survives a toast re-triggered under the cursor', async () => {
  const port = nextPort++;
  const app = makeApp({ port });
  try {
    await app.waitReady();
    eq((await app.post('show-toast', { kind: 'text' })).visible, true);
    await app.post('toast-hover', { over: true });
    // a second toast lands while the cursor is still parked on the window
    await app.post('show-toast', { kind: 'text' });
    await app.sleep(5000); // > 4.2s default TTL
    eq((await app.post('toast-visible', {})).visible, true, 'paused while hovered');
    await app.post('toast-hover', { over: false });
    await app.sleep(3000); // > 2.5s after-hover re-arm
    eq((await app.post('toast-visible', {})).visible, false, 'hides after mouse leaves');
  } finally { await app.stop(); }
});

test('overlay: shown only after the renderer painted, at exact display bounds', async () => {
  const port = nextPort++;
  const app = makeApp({ port });
  try {
    await app.waitReady();
    await app.post('trigger', { mode: 'region' });
    const timing = await app.post('overlay-timing', {});
    const get = (label) => {
      const hit = (timing || []).find((s) => s.startsWith(label + '='));
      return hit ? Number(hit.split('=')[1]) : null;
    };
    assert(get('painted') !== null, 'painted mark present (renderer acked): ' + JSON.stringify(timing));
    assert(get('show') !== null, 'show mark present');
    assert(get('painted') <= get('show'), 'painted before show: ' + JSON.stringify(timing));
    // will-resize preventDefault must not break programmatic setBounds: every
    // display gets an overlay at exactly its bounds (+ ghost offset on x)
    const st = await app.post('state', {});
    for (const d of st.displays) {
      const hit = st.overlayBounds.find((b) => b.x === d.bounds.x + 20000 && b.y === d.bounds.y);
      assert(hit, `overlay for display at ${d.bounds.x},${d.bounds.y}: ` + JSON.stringify(st.overlayBounds));
      eq({ w: hit.width, h: hit.height }, { w: d.bounds.width, h: d.bounds.height },
        'overlay covers the display exactly');
    }
  } finally { await app.stop(); }
});

module.exports = {};
