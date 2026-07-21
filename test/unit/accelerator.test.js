'use strict';
// Unit tests for the hotkey-recorder accelerator builder (same file the
// renderer loads).
const path = require('path');
const { test, eq } = require('../tiny');
const { toAccelerator } = require(path.join(__dirname, '..', '..', 'src', 'app', 'accelerator.js'));

const ev = (o) => ({ ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, key: '', code: '', ...o });

test('Ctrl+Shift+4 builds from layout-independent code', () => {
  eq(toAccelerator(ev({ ctrlKey: true, shiftKey: true, key: '$', code: 'Digit4' }), 'win32'), 'Ctrl+Shift+4');
});

test('meta maps to Cmd on darwin (matches the Cmd+… defaults)', () => {
  eq(toAccelerator(ev({ metaKey: true, shiftKey: true, key: '4', code: 'Digit4' }), 'darwin'), 'Cmd+Shift+4');
});

test('meta maps to Super elsewhere', () => {
  eq(toAccelerator(ev({ metaKey: true, key: 's', code: 'KeyS' }), 'win32'), 'Super+S');
});

test('modifier-only press returns null (recorder keeps waiting)', () => {
  eq(toAccelerator(ev({ ctrlKey: true, key: 'Control', code: 'ControlLeft' }), 'win32'), null);
});

test('PrintScreen works without modifiers', () => {
  eq(toAccelerator(ev({ key: 'PrintScreen', code: 'PrintScreen' }), 'win32'), 'PrintScreen');
});

test('F-keys work without modifiers', () => {
  eq(toAccelerator(ev({ key: 'F6', code: 'F6' }), 'win32'), 'F6');
});

test('a bare letter is rejected (needs a modifier)', () => {
  eq(toAccelerator(ev({ key: 'a', code: 'KeyA' }), 'win32'), null);
});

test('RU layout letter still resolves via code', () => {
  eq(toAccelerator(ev({ ctrlKey: true, altKey: true, key: 'е', code: 'KeyT' }), 'win32'), 'Ctrl+Alt+T');
});
