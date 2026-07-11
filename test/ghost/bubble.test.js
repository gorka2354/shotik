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

// The translate popup must survive a transient blur right after showing (focus
// settling) — otherwise it flashes and vanishes before the result lands. This is
// the other half of the "translate then vanish" bug.
test('popup survives a blur inside the grace window', async () => {
  await withApp({ SHOTIK_TP_GRACE: '5000' }, async ({ post }) => {
    await post('selection-bubble', { text: 'hello', x: 900, y: 500 });
    await post('bubble-activate', { wait: 40 });
    const r = await post('blur-tp', {}); // blur fires well inside the 5s grace
    assert(r.popupVisible === true, 'popup kept despite an immediate blur');
  });
});

// The real EN→RU bug: a slower translation meant the popup could blur+hide while
// still "loading", so the result never showed. It must NOT dismiss while loading.
test('popup ignores blur while still loading (slow-translation bug)', async () => {
  await withApp({ SHOTIK_TP_GRACE: '0' }, async ({ post }) => {
    await post('show-tp-loading', { text: 'hello' });
    const mid = await post('blur-tp', {});         // blur arrives before the result
    assert(mid.popupVisible === true, 'loading popup survives blur');
    await post('resolve-tp', { text: 'hello', translated: 'привет' }); // result lands
    const after = await post('blur-tp', {});        // now a click-away (grace 0) closes it
    assert(after.popupVisible === false, 'after the result, blur dismisses normally');
  });
});

test('popup still dismisses on a genuine (post-grace) blur', async () => {
  await withApp({ SHOTIK_TP_GRACE: '0' }, async ({ post, sleep }) => {
    await post('selection-bubble', { text: 'hello', x: 900, y: 500 });
    await post('bubble-activate', { wait: 40 });
    await sleep(450); // let the result render + its tp:resize settle first
    const r = await post('blur-tp', {}); // grace 0 → any blur hides (click-away still works)
    assert(r.popupVisible === false, 'popup dismissed when blur is past the grace');
  });
});

module.exports = {};
