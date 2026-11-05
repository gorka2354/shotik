'use strict';
// Contract tests of the built binary: `npm test` (unit + ghost) · `npm test unit` · `npm test ghost`.
// Rust's own tests run with `cargo test --workspace`.
// Zero deps. Exits non-zero if anything fails so it can gate a release.
const { runRegistered } = require('./tiny');

const which = process.argv[2] || 'all';

(async () => {
  let passed = 0, failed = 0;

  if (which === 'all' || which === 'unit') {
    console.log('\n── unit ──');
    require('./unit/ghost-adapter.test.js');
    require('./unit/sign-update.test.js');
    const r = await runRegistered('unit › ');
    passed += r.passed; failed += r.failed;
  }

  if (which === 'all' || which === 'ghost') {
    console.log('\n── ghost integration ──');
    require('./ghost/bubble.test.js');
    require('./ghost/ocr.test.js');
    require('./ghost/texttab.test.js');
    require('./ghost/mcp-tools.test.js');
    require('./ghost/mcp-recordings.test.js');
    require('./ghost/recorder.test.js');
    require('./ghost/recording-seek.test.js');
    require('./ghost/audio-recording.test.js');
    require('./ghost/update.test.js');
    const r = await runRegistered('ghost › ');
    passed += r.passed; failed += r.failed;
  }

  console.log(`\n${failed ? '✗' : '✓'} ${passed} passed, ${failed} failed\n`);
  process.exit(failed ? 1 : 0);
})();
