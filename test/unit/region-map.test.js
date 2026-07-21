'use strict';
// Unit tests for the virtual-desktop region mapping (pure, no Electron).
const path = require('path');
const { test, assert, eq } = require('../tiny');
const { validateRegion, mapGlobalRegion } = require(path.join(__dirname, '..', '..', 'src', 'main', 'region-map.js'));

// The reporter's real geometry: primary 2560x1440 at 0,0; secondary 1920x1080 at 2560,261.
const DISPLAYS = [
  { id: 1836973850, bounds: { x: 0, y: 0, width: 2560, height: 1440 } },
  { id: 1609006897, bounds: { x: 2560, y: 261, width: 1920, height: 1080 } },
];

test('region on the secondary monitor maps to it with relative coords', () => {
  const m = mapGlobalRegion(DISPLAYS, { x: 2560, y: 261, width: 1900, height: 1060 });
  eq(m.displayId, 1609006897);
  eq(m.relRect, { x: 0, y: 0, width: 1900, height: 1060 });
  eq(m.clipped, false);
});

test('region on the primary keeps global == relative (origin 0,0)', () => {
  const m = mapGlobalRegion(DISPLAYS, { x: 100, y: 200, width: 300, height: 150 });
  eq(m.displayId, 1836973850);
  eq(m.relRect, { x: 100, y: 200, width: 300, height: 150 });
});

test('negative origin displays work (window at x=-8)', () => {
  const disp = [{ id: 7, bounds: { x: -1920, y: -120, width: 1920, height: 1080 } }];
  const m = mapGlobalRegion(disp, { x: -1900, y: -100, width: 400, height: 300 });
  eq(m.displayId, 7);
  eq(m.relRect, { x: 20, y: 20, width: 400, height: 300 });
});

test('a region spanning both monitors clips to the larger share and says so', () => {
  // 1000px wide straddling the 2560 boundary: 300px on primary, 700px on secondary
  const m = mapGlobalRegion(DISPLAYS, { x: 2260, y: 400, width: 1000, height: 200 });
  eq(m.displayId, 1609006897);
  eq(m.globalRect, { x: 2560, y: 400, width: 700, height: 200 });
  eq(m.clipped, true);
});

test('region hanging off the bottom edge is clipped, not rejected', () => {
  const m = mapGlobalRegion(DISPLAYS, { x: 100, y: 1400, width: 200, height: 200 });
  eq(m.globalRect, { x: 100, y: 1400, width: 200, height: 40 });
  eq(m.clipped, true);
});

test('region outside every display throws with display list in the message', () => {
  let err = null;
  try { mapGlobalRegion(DISPLAYS, { x: 9000, y: 9000, width: 100, height: 100 }); } catch (e) { err = e; }
  assert(err, 'throws');
  assert(/does not intersect/.test(err.message), err.message);
  assert(/1836973850/.test(err.message), 'lists displays: ' + err.message);
});

test('display_id restricts mapping to that display', () => {
  // rect is mostly on secondary, but the caller pins the primary
  const m = mapGlobalRegion(DISPLAYS, { x: 2260, y: 400, width: 1000, height: 200 }, 1836973850);
  eq(m.displayId, 1836973850);
  eq(m.globalRect, { x: 2260, y: 400, width: 300, height: 200 });
});

test('unknown display_id throws', () => {
  let err = null;
  try { mapGlobalRegion(DISPLAYS, { x: 0, y: 0, width: 10, height: 10 }, 42); } catch (e) { err = e; }
  assert(err && /Unknown display_id 42/.test(err.message), err && err.message);
});

// -- validation: the NaN -> empty PNG class of bugs --

test('missing fields are rejected with a clear message (not NaN)', () => {
  let err = null;
  try { validateRegion({ region: { x: 1, y: 2, width: 3, height: 4 } }); } catch (e) { err = e; }
  assert(err && /"x" must be a finite number/.test(err.message), err && err.message);
});

test('numeric strings are accepted (client serialization quirks)', () => {
  eq(validateRegion({ x: '2560', y: '261', width: '100', height: '50.4' }),
    { x: 2560, y: 261, width: 100, height: 50 });
});

test('zero/negative size is rejected', () => {
  let err = null;
  try { validateRegion({ x: 0, y: 0, width: 0, height: 10 }); } catch (e) { err = e; }
  assert(err && /width and height must be >= 1/.test(err.message), err && err.message);
});

test('non-numeric garbage is rejected', () => {
  let err = null;
  try { validateRegion({ x: 'left', y: 0, width: 10, height: 10 }); } catch (e) { err = e; }
  assert(err && /finite number/.test(err.message), err && err.message);
});
