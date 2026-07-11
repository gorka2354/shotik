'use strict';
// Unit tests for the translation layer. Mocks global.fetch and the settings
// module so it runs in plain node (no Electron). Covers the response parsing
// that the auto-translate bubble depends on — incl. the EN→RU path that broke.
const path = require('path');
const { test, assert, eq } = require('../tiny');

const SRC = path.join(__dirname, '..', '..', 'src', 'main');

// --- inject a fake ./settings into require.cache before loading translate.js ---
const settingsPath = require.resolve(path.join(SRC, 'settings.js'));
let fakeCfg = { target: 'ru', provider: 'free', deeplKey: '' };
require.cache[settingsPath] = { id: settingsPath, filename: settingsPath, loaded: true,
  exports: { get: () => ({ translate: fakeCfg }) } };

const translate = require(path.join(SRC, 'translate.js'));
const { freeTranslate, deeplTranslate } = translate._internal;

// --- fetch mock ---
let fetchImpl = null;
global.fetch = (...args) => fetchImpl(...args);
const jsonRes = (body) => ({ ok: true, status: 200, json: async () => body, text: async () => JSON.stringify(body) });

// Google's real shape for "hello"→ru: [[["привет","hello",...]],null,"en",...]
test('freeTranslate parses a single-segment Google response (en→ru)', async () => {
  fetchImpl = async (url) => { assert(url.includes('tl=ru'), 'target in url'); return jsonRes([[['привет', 'hello', null, null, 10]], null, 'en']); };
  const r = await freeTranslate('hello', 'ru');
  eq(r.text, 'привет'); eq(r.source, 'en'); eq(r.provider, 'free');
});

test('freeTranslate joins multiple segments', async () => {
  fetchImpl = async () => jsonRes([[['Привет, ', 'Hi, '], ['мир', 'world']], null, 'en']);
  const r = await freeTranslate('Hi, world', 'ru');
  eq(r.text, 'Привет, мир');
});

test('freeTranslate handles same-language (ru→ru) echo', async () => {
  fetchImpl = async () => jsonRes([[['привет', 'привет', null, null, 5]], null, 'ru']);
  const r = await freeTranslate('привет', 'ru');
  eq(r.text, 'привет'); eq(r.source, 'ru');
});

test('freeTranslate tolerates a malformed data[0]', async () => {
  fetchImpl = async () => jsonRes([null, null, 'en']);
  const r = await freeTranslate('x', 'ru');
  eq(r.text, '');
});

test('freeTranslate throws on non-200', async () => {
  fetchImpl = async () => ({ ok: false, status: 429, json: async () => ({}), text: async () => '' });
  let threw = false; try { await freeTranslate('hello', 'ru'); } catch (_) { threw = true; }
  assert(threw, 'should throw on HTTP 429');
});

test('deeplTranslate parses translations[0] and picks free host for :fx key', async () => {
  let seenHost = '';
  fetchImpl = async (url) => { seenHost = url; return jsonRes({ translations: [{ text: 'привет', detected_source_language: 'EN' }] }); };
  const r = await deeplTranslate('hello', 'ru', 'abc:fx');
  eq(r.text, 'привет'); eq(r.source, 'en'); eq(r.provider, 'deepl');
  assert(seenHost.startsWith('https://api-free.deepl.com'), 'free host for :fx key');
});

test('translate() returns stub output when SHOTIK_TRANSLATE_STUB=1', async () => {
  // stub is read at module load; re-require in a child-safe way via cache bust
  const p = require.resolve(path.join(SRC, 'translate.js'));
  delete require.cache[p];
  process.env.SHOTIK_TRANSLATE_STUB = '1';
  const t2 = require(path.join(SRC, 'translate.js'));
  const r = await t2.translate('hello', 'ru');
  eq(r.text, '‹ru› hello'); eq(r.provider, 'stub');
  delete process.env.SHOTIK_TRANSLATE_STUB;
  delete require.cache[p];
});

test('translate() short-circuits empty input', async () => {
  const r = await translate.translate('   ');
  eq(r.text, ''); eq(r.provider, 'none');
});

test('translate() uses the settings target and free provider end to end', async () => {
  fakeCfg = { target: 'ru', provider: 'free', deeplKey: '' };
  fetchImpl = async (url) => { assert(url.includes('tl=ru')); return jsonRes([[['привет', 'hello']], null, 'en']); };
  const r = await translate.translate('hello');
  eq(r.text, 'привет');
});

test('translate() falls back to free when a DeepL key call fails', async () => {
  fakeCfg = { target: 'ru', provider: 'deepl', deeplKey: 'bad:fx' };
  let calls = 0;
  fetchImpl = async (url) => {
    calls++;
    if (url.includes('deepl.com')) return { ok: false, status: 403, json: async () => ({}), text: async () => '' };
    return jsonRes([[['привет', 'hello']], null, 'en']);
  };
  const r = await translate.translate('hello');
  eq(r.text, 'привет'); eq(r.provider, 'free', 'fell through to free'); assert(calls === 2, 'tried deepl then free');
});

module.exports = {};
