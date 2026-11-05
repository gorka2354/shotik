'use strict';
// Long-running memory check: is there a leak, and does it depend on activity?
//
//   node bench/soak.js --impl electron --exe node_modules/electron/dist/electron.exe \
//     --entry bench/soak-entry.js --hours 6 --out <file.jsonl>
//
// Two ghost instances with separate profiles run side by side. `active` takes a real overlay session
// and a real screenshot every cycle; `idle` only sits there, like the app in the tray. Real frames
// reach a temporary directory only, emptied every cycle and deleted at the end.
const fs = require('fs');
const os = require('os');
const path = require('path');
const lib = require('./lib');
const adapters = require('./adapters');

const { sleep, post, mcp, textOf, launch, waitReady, stop, snapshot } = lib;

function arg(name, fallback = null) {
  const i = process.argv.indexOf('--' + name);
  return i === -1 ? fallback : process.argv[i + 1];
}

const IMPL = adapters[arg('impl', 'rust')];
const EXE = path.resolve(arg('exe'));
const HOURS = Number(arg('hours', 6));
const CYCLE_MS = Number(arg('cycle', 60)) * 1000;
const SAMPLE_MS = Number(arg('sample', 300)) * 1000;
const OUT = path.resolve(arg('out', 'soak.jsonl'));

function empty(dir) {
  for (const f of fs.readdirSync(dir)) {
    try { fs.rmSync(path.join(dir, f), { recursive: true, force: true }); } catch (_) { /* in use, next cycle */ }
  }
}

async function main() {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'shotik-soak-'));
  const start = (name, port) => {
    const dir = path.join(tmp, name);
    fs.mkdirSync(path.join(dir, 'saved'), { recursive: true });
    const args = IMPL.args;
    const app = launch({ exe: EXE, args, cwd: path.dirname(EXE), port, env: { ...IMPL.env, APPDATA: dir, TEMP: dir, TMP: dir } });
    return { name, app, port, saved: path.join(dir, 'saved') };
  };
  const active = start('active', Number(arg('port', 7611)));
  const idle = start('idle', active.port + 1);
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  const write = (o) => fs.appendFileSync(OUT, JSON.stringify(o) + '\n');
  try {
    for (const inst of [active, idle]) {
      await waitReady(inst.app);
      await IMPL.setSettings(inst.port, { saveDir: inst.saved.replace(/\\/g, '/') });
    }
    const primary = JSON.parse(textOf(await mcp(active.port, 'list_displays', {}))).find((d) => d.primary);
    const t0 = Date.now();
    let nextSample = 0;
    let cycles = 0;
    let failures = 0;
    while (Date.now() - t0 < HOURS * 3600e3) {
      if (Date.now() >= nextSample) {
        nextSample = Date.now() + SAMPLE_MS;
        for (const inst of [active, idle]) {
          const s = snapshot(inst.app.child.pid);
          write({
            t: Math.round((Date.now() - t0) / 1000), name: inst.name,
            cycles: inst === active ? cycles : 0, failures: inst === active ? failures : 0,
            procs: s.procs, rootPrivateMB: s.rootPrivateMB, treePrivateMB: s.treePrivateMB, treeWorkingSetMB: s.treeWorkingSetMB, treeHandles: s.treeHandles,
          });
        }
      }
      const cycleStart = Date.now();
      try {
        await post(active.port, 'trigger', { mode: 'region' });
        await IMPL.cancelOverlay(active.port);
        await mcp(active.port, 'take_screenshot', { display_id: primary.id });
        cycles++;
      } catch (e) {
        failures++;
        write({ t: Math.round((Date.now() - t0) / 1000), name: 'active', error: e.message });
      }
      empty(active.saved);
      await sleep(Math.max(0, CYCLE_MS - (Date.now() - cycleStart)));
    }
  } finally {
    await stop(active.app);
    await stop(idle.app);
    await sleep(500);
    try { fs.rmSync(tmp, { recursive: true, force: true }); } catch (e) { console.error('could not delete', tmp, e.message); }
  }
}

main().catch((e) => { console.error('SOAK FAILED:', e.stack || e.message); process.exit(1); });
