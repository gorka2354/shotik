'use strict';
// Ghost integration for the auto-translate selection bubble. Drives the same
// code paths the real watcher would, over HTTP. Each exported test gets a fresh
// app (so state can't leak between cases).
const { makeApp } = require('./harness');
const { test, assert, eq } = require('../tiny');

const PORT = 7495;

// helper: run a body with a booted app, always tear down
async function withApp(env, body) {
  const app = makeApp({ port: PORT, env });
  try { await app.waitReady(); await body(app); }
  finally { await app.stop(); }
}

test('bubble appears on a real selection and is off-screen (ghost)', async () => {
  await withApp({}, async ({ post }) => {
    const r = await post('selection-bubble', { text: 'Good morning, please review the PR', x: 900, y: 500 });
    assert(r.bubble === true, 'bubble shown');
    assert(r.bounds && r.bounds.x >= 20000, 'bubble is off-screen at +20000, got ' + JSON.stringify(r.bounds));
  });
});

test('bubble does NOT appear for an empty / whitespace selection', async () => {
  await withApp({}, async ({ post }) => {
    await post('bubble-clear', {});
    const r = await post('selection-bubble', { text: '   ' });
    eq(r.bubble, false, 'no bubble for whitespace-only selection');
  });
});

test('clicking the bubble translates and shows the popup with the text', async () => {
  await withApp({}, async ({ post }) => {
    await post('selection-bubble', { text: 'hello', x: 900, y: 500 });
    const a = await post('bubble-activate', {});
    assert(a.popup === true, 'popup opened');
    const c = await post('exec-js', { target: 'tp', code:
      'JSON.stringify({o:document.getElementById("orig").textContent,t:document.getElementById("trans").textContent})' });
    const parsed = JSON.parse(c);
    eq(parsed.o, 'hello', 'original preserved');
    assert(parsed.t.includes('hello'), 'stub translation contains the text, got ' + parsed.t);
  });
});

// Regression: a `clear` racing in between show and click must NOT lose the text.
// (The watcher can emit a transient clear when focus flickers; the click still
// has to translate.) This is the "button vanished, nothing happened" bug.
test('clear before activate still translates (clear-race regression)', async () => {
  await withApp({}, async ({ post }) => {
    await post('selection-bubble', { text: 'world', x: 900, y: 500 });
    await post('inject-clear', {});            // real clear path (hides bubble, keeps text)
    const a = await post('bubble-activate', {}); // a click that raced with the clear
    assert(a.popup === true, 'popup still opens after a racing clear');
    const c = await post('exec-js', { target: 'tp', code: 'document.getElementById("orig").textContent' });
    eq(c, 'world', 'the pre-clear selection text survived');
  });
});

// The popup must NOT dismiss on window blur (that flaky focus behaviour was the
// "translate then vanish" bug). While loading it stays; after the result it
// auto-hides on a timer that hovering pauses.
test('popup stays up while loading, does not vanish before the result', async () => {
  await withApp({ SHOTIK_TP_TTL: '300' }, async ({ post, sleep }) => {
    await post('show-tp-loading', { text: 'hello' });
    await sleep(500); // longer than the TTL — loading must never auto-hide
    const r = await post('tp-visible', {});
    assert(r.popupVisible === true, 'loading popup did not vanish before the result');
  });
});

test('popup auto-hides a while after the result (no blur needed)', async () => {
  await withApp({ SHOTIK_TP_TTL: '1500' }, async ({ post, sleep }) => {
    await post('selection-bubble', { text: 'hello', x: 900, y: 500 });
    await post('bubble-activate', { wait: 500 }); // let the fresh popup finish loading + show
    const shown = await post('tp-visible', {});
    assert(shown.popupVisible === true, 'result is shown');
    await sleep(1300); // past the 1500ms TTL (which started at the result)
    const later = await post('tp-visible', {});
    assert(later.popupVisible === false, 'popup auto-hid after the TTL');
  });
});

test('hovering the popup pauses its auto-hide', async () => {
  await withApp({ SHOTIK_TP_TTL: '400' }, async ({ post, sleep }) => {
    await post('selection-bubble', { text: 'hello', x: 900, y: 500 });
    await post('bubble-activate', { wait: 20 });
    await post('tp-hover', { over: true });   // pointer on the popup
    await sleep(800);                         // well past the TTL
    const hovered = await post('tp-visible', {});
    assert(hovered.popupVisible === true, 'popup stays while hovered');
    await post('tp-hover', { over: false });  // pointer leaves → countdown resumes
    await sleep(800);
    const left = await post('tp-visible', {});
    assert(left.popupVisible === false, 'popup hides after the pointer leaves');
  });
});

module.exports = {};
