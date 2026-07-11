'use strict';
// Test runner: `npm test` (unit + ghost) · `npm test unit` · `npm test ghost`.
// Zero deps. Exits non-zero if anything fails so it can gate a release.
const { execFileSync } = require('child_process');
const { runRegistered } = require('./tiny');

const which = process.argv[2] || 'all';

function killStrayElectron() {
  // never touches the user's installed Shotik (that runs as Shotik.exe)
  try { execFileSync('taskkill', ['/F', '/IM', 'electron.exe'], { stdio: 'ignore' }); } catch (_) {}
}

(async () => {
  let passed = 0, failed = 0;

  if (which === 'all' || which === 'unit') {
    console.log('\n── unit ──');
    require('./unit/translate.test.js');
    require('./unit/highlight.test.js');
    const r = await runRegistered('unit › ');
    passed += r.passed; failed += r.failed;
  }

  if (which === 'all' || which === 'ghost') {
    console.log('\n── ghost integration ──');
    killStrayElectron();
    require('./ghost/bubble.test.js');
    require('./ghost/ocr.test.js');
    require('./ghost/texttab.test.js');
    const r = await runRegistered('ghost › ');
    passed += r.passed; failed += r.failed;
    killStrayElectron();
  }

  console.log(`\n${failed ? '✗' : '✓'} ${passed} passed, ${failed} failed\n`);
  process.exit(failed ? 1 : 0);
})();
