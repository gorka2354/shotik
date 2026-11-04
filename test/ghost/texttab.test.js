'use strict';
// The Text tab's manual "quick translate": type text, click Translate, get a
// result. Drives the real main-window UI with semantic actions (stub translator).
const { makeApp } = require('./adapter');
const { test, assert } = require('../tiny');


test('Text tab quick-translate returns a translation', async () => {
  const app = makeApp();
  try {
    await app.waitReady();
    // switch to the Text tab, type, and press Translate
    await app.uiAction('main', { type: 'quick-translate', text: 'hello world' });
    let text = '';
    for (let i = 0; i < 25; i++) {
      await app.sleep(150);
      text = (await app.uiState('main')).text.result;
      if (text && !/Translating|Перевожу|…/.test(text)) break;
    }
    assert(/hello world/.test(text), 'result carries the (stub) translation, got: ' + JSON.stringify(text));
    const visible = (await app.uiState('main')).text.resultVisible;
    assert(visible === true, 'result panel is shown');
  } finally {
    await app.stop();
  }
});

module.exports = {};
