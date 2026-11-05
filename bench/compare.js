'use strict';
// A before and after table from two benchmark results:
//   node bench/compare.js bench/results/electron-1.3.1.json bench/results/rust-2.0.0.json
const fs = require('fs');

const [a, b] = process.argv.slice(2).map((file) => JSON.parse(fs.readFileSync(file, 'utf8')));
const get = (o, path) => path.split('.').reduce((v, k) => (v == null ? v : v[k]), o);

const ROWS = [
  ['Executable, MB', 'artifact.exeMB', 'less'],
  ['Install folder, MB', 'artifact.installDirMB', 'less'],
  ['Startup to ready, median ms', 'startup.readyMs.median', 'less'],
  ['Idle processes', 'idle.procs', 'less'],
  ['Idle memory, private MB', 'idle.treePrivateMB', 'less'],
  ['Idle CPU, ms per s', 'idle.cpuMsPerSec', 'less'],
  ['Overlay shown, cold ms', 'overlay.coldShowMs', 'less'],
  ['Overlay shown, warm median ms', 'overlay.warmShowMs.median', 'less'],
  ['Overlay shown, warm p95 ms', 'overlay.warmShowMs.p95', 'less'],
  ['Capture of the display, warm median ms', 'overlay.warmStagesMs.grab.median', 'less'],
  ['Memory with overlays open, private MB', 'overlay.peakTreePrivateMB', 'less'],
  ['Memory after overlays closed, private MB', 'overlay.treePrivateAfterSettleMB', 'less'],
  ['take_screenshot, median ms', 'mcp.takeScreenshotMs.median', 'less'],
  ['take_screenshot full resolution, median ms', 'mcp.takeScreenshotFullResMs.median', 'less'],
  ['list_windows, median ms', 'mcp.listWindowsMs.median', 'less'],
  ['capture_window, median ms', 'mcp.captureWindowMs.median', 'less'],
  ['OCR for a selection, median ms', 'ocr.selectionMs.median', 'less'],
  ['Video: start, ms', 'video.startMs', 'less'],
  ['Video: stop, ms', 'video.stopMs', 'less'],
  ['Video: CPU, ms per s', 'video.cpuMsPerSec', 'less'],
  ['Video: peak memory, private MB', 'video.peakTreePrivateMB', 'less'],
  ['Video: size, MB per minute', 'video.mbPerMinute', 'less'],
  ['GIF export of the clip, ms', 'gif.exportMs', 'less'],
  ['Audio: start, ms', 'audio.startMs', 'less'],
  ['Audio: CPU, ms per s', 'audio.cpuMsPerSec', 'less'],
  ['Memory at the end of the run, private MB', 'afterAll.treePrivateMB', 'less'],
];

console.log(`| Metric | ${a.label} | ${b.label} | Change |`);
console.log('|---|---:|---:|---:|');
let worse = 0;
for (const [name, path, better] of ROWS) {
  const x = get(a, path);
  const y = get(b, path);
  let change = '';
  if (typeof x === 'number' && typeof y === 'number' && x !== 0) {
    const ratio = y / x;
    change = ratio >= 1 ? `+${Math.round((ratio - 1) * 100)}%` : `${ratio.toFixed(2)}x`;
    if (better === 'less' && ratio > 1.1) { worse++; change += ' worse'; }
  }
  console.log(`| ${name} | ${x ?? 'n/a'} | ${y ?? 'n/a'} | ${change} |`);
}
console.log(`\nMetrics more than 10% worse: ${worse}`);
