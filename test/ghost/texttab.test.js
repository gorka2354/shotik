'use strict';
// The Text tab's manual "quick translate": type text, click Translate, get a
// result. Drives the real main-window UI over exec-js (stub translator).
const { makeApp } = require('./harness');
const { test, assert } = require('../tiny');

const PORT = 7497;

test('Text tab quick-translate returns a translation', async () => {
  const app = makeApp({ port: PORT });
  try {
    await app.waitReady();
    // switch to the Text tab, type, and press Translate
    await app.post('exec-js', { target: 'main', code:
      'document.querySelector(\'[data-page="text"]\').click();' +
      'document.querySelector("#txtInput").value="hello world";' +
      'document.querySelector("#txtRun").click(); "ok"' });
    let text = '';
    for (let i = 0; i < 25; i++) {
      await app.sleep(150);
      text = await app.post('exec-js', { target: 'main', code: 'document.querySelector("#txtResult").textContent' });
      if (text && !/Translating|Перевожу|…/.test(text)) break;
    }
    assert(/hello world/.test(text), 'result carries the (stub) translation, got: ' + JSON.stringify(text));
    const visible = await app.post('exec-js', { target: 'main', code: '!document.querySelector("#txtResultWrap").hidden' });
    assert(visible === true, 'result panel is shown');
  } finally {
    await app.stop();
  }
});

module.exports = {};
