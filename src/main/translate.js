'use strict';
// Text translation. Default: free public endpoint (no key, best-effort).
// Optional: DeepL by API key (reliable). Runs in the main process (no CSP/CORS).
const settings = require('./settings');

const STUB = process.env.SHOTIK_TRANSLATE_STUB === '1'; // deterministic output for tests

function withTimeout(ms) {
  const ctrl = new AbortController();
  const id = setTimeout(() => ctrl.abort(), ms);
  return { signal: ctrl.signal, done: () => clearTimeout(id) };
}

// Free, unofficial Google endpoint. No key; may rate-limit or change.
async function freeTranslate(text, target) {
  const t = withTimeout(12000);
  try {
    const url = 'https://translate.googleapis.com/translate_a/single?client=gtx&sl=auto&tl='
      + encodeURIComponent(target) + '&dt=t&q=' + encodeURIComponent(text);
    const res = await fetch(url, {
      signal: t.signal,
      headers: { 'User-Agent': 'Mozilla/5.0', 'Accept': 'application/json' },
    });
    if (!res.ok) throw new Error('HTTP ' + res.status);
    const data = await res.json();
    const out = (data[0] || []).map((seg) => (seg && seg[0]) ? seg[0] : '').join('');
    return { text: out, source: data[2] || 'auto', provider: 'free' };
  } finally { t.done(); }
}

// DeepL API (free or pro key). Reliable; requires a key.
async function deeplTranslate(text, target, key) {
  const t = withTimeout(12000);
  try {
    const host = key.trim().endsWith(':fx') ? 'https://api-free.deepl.com' : 'https://api.deepl.com';
    const res = await fetch(host + '/v2/translate', {
      method: 'POST',
      signal: t.signal,
      headers: { 'Authorization': 'DeepL-Auth-Key ' + key.trim(), 'Content-Type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({ text, target_lang: target.toUpperCase() }).toString(),
    });
    if (!res.ok) throw new Error('DeepL HTTP ' + res.status);
    const data = await res.json();
    const tr = (data.translations && data.translations[0]) || {};
    return { text: tr.text || '', source: (tr.detected_source_language || 'auto').toLowerCase(), provider: 'deepl' };
  } finally { t.done(); }
}

// translate(text, target?) — target defaults to settings.translate.target.
async function translate(text, target) {
  const cfg = settings.get().translate || {};
  const tl = (target || cfg.target || 'en').toLowerCase();
  if (!text || !text.trim()) return { text: '', source: 'auto', provider: 'none' };
  if (STUB) return { text: `‹${tl}› ` + text, source: 'stub', provider: 'stub' };

  const key = (cfg.deeplKey || '').trim();
  if (cfg.provider === 'deepl' && key) {
    try { return await deeplTranslate(text, tl, key); }
    catch (e) { /* fall through to free on failure */ }
  }
  return await freeTranslate(text, tl);
}

// _internal exported for unit tests (freeTranslate/deeplTranslate are pure —
// they take text+target and a mockable global fetch, no settings needed).
module.exports = { translate, _internal: { freeTranslate, deeplTranslate } };
