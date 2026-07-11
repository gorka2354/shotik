'use strict';
// Minimal zero-dep test helpers (the project has no dev test framework).
// Usage: const { test, assert, eq, near } = require('../tiny');
const registry = [];

function test(name, fn) { registry.push({ name, fn }); }

function assert(cond, msg) { if (!cond) throw new Error(msg || 'assertion failed'); }

function eq(actual, expected, msg) {
  const a = JSON.stringify(actual), b = JSON.stringify(expected);
  if (a !== b) throw new Error((msg ? msg + ': ' : '') + `expected ${b}, got ${a}`);
}

function near(actual, expected, tol, msg) {
  if (Math.abs(actual - expected) > tol) throw new Error((msg ? msg + ': ' : '') + `expected ~${expected}±${tol}, got ${actual}`);
}

// Run everything registered so far; returns {passed, failed}. Clears the registry.
async function runRegistered(prefix) {
  let passed = 0, failed = 0;
  const items = registry.splice(0);
  for (const { name, fn } of items) {
    try { await fn(); console.log(`  ✓ ${prefix}${name}`); passed++; }
    catch (e) { console.log(`  ✗ ${prefix}${name}\n      ${e.message}`); failed++; }
  }
  return { passed, failed };
}

module.exports = { test, assert, eq, near, runRegistered };
