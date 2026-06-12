'use strict';
const { app } = require('electron');
const settings = require('./settings');
const { DICTS, resolveLang, makeT } = require('./strings');

function lang() {
  let sys = 'en';
  try { sys = app.getLocale(); } catch (_) {}
  return resolveLang(settings.get().language, sys);
}
function t(key, ...args) { return makeT(lang())(key, ...args); }
function dict() { return DICTS[lang()] || DICTS.en; }

module.exports = { t, lang, dict };
