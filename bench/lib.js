'use strict';
// Benchmark plumbing: starting an instance, HTTP to /test/*, memory and CPU of its process tree.
const { spawn, execFileSync } = require('child_process');
const http = require('http');

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function post(port, route, body, timeoutMs = 120000) {
  return new Promise((resolve, reject) => {
    const data = Buffer.from(JSON.stringify(body || {}));
    const req = http.request({
      host: '127.0.0.1', port, path: '/test/' + route, method: 'POST', timeout: timeoutMs,
      headers: { 'Content-Type': 'application/json', 'Content-Length': data.length },
    }, (res) => {
      let b = '';
      res.on('data', (c) => { b += c; });
      res.on('end', () => {
        let j;
        try { j = JSON.parse(b); } catch (_) { return reject(new Error(`${route}: not JSON: ${b.slice(0, 200)}`)); }
        if (j.ok === false) return reject(new Error(`${route}: ${j.error}`));
        resolve(j && typeof j === 'object' && 'result' in j ? j.result : j);
      });
    });
    req.on('timeout', () => req.destroy(new Error(`${route}: timed out after ${timeoutMs} ms`)));
    req.on('error', reject);
    req.write(data);
    req.end();
  });
}

function mcp(port, name, args, timeoutMs = 120000) {
  return new Promise((resolve, reject) => {
    const data = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args || {} } }));
    const req = http.request({
      host: '127.0.0.1', port, path: '/mcp', method: 'POST', timeout: timeoutMs,
      headers: { 'Content-Type': 'application/json', 'Content-Length': data.length },
    }, (res) => {
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () => {
        try {
          const out = JSON.parse(Buffer.concat(chunks).toString('utf8')).result;
          if (out.isError) return reject(new Error(`${name}: ${(out.content[0] || {}).text}`));
          resolve(out);
        } catch (e) { reject(new Error(`${name}: bad reply: ${e.message}`)); }
      });
    });
    req.on('timeout', () => req.destroy(new Error(`${name}: timed out after ${timeoutMs} ms`)));
    req.on('error', reject);
    req.write(data);
    req.end();
  });
}

const textOf = (res) => ((res.content || []).find((c) => c.type === 'text') || {}).text || '';

function launch({ exe, args, cwd, port, env }) {
  const childEnv = {
    ...process.env,
    SHOTIK_TEST: '1', SHOTIK_GHOST: '1', SHOTIK_TRANSLATE_STUB: '1', SHOTIK_PORT: String(port),
    ...env,
  };
  // SHOTIK_FAKE_SCREEN must not leak in from the environment when measuring real capture
  if (!env || !env.SHOTIK_FAKE_SCREEN) delete childEnv.SHOTIK_FAKE_SCREEN;
  const started = process.hrtime.bigint();
  const child = spawn(exe, args, { cwd, env: childEnv, stdio: ['ignore', 'ignore', 'ignore'], windowsHide: true });
  return { child, port, started };
}

// Time from process start to the first /test/ping reply.
async function waitReady(app, timeoutMs = 60000) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    try {
      if (await post(app.port, 'ping', {}, 1000) === 'pong') {
        return Number(process.hrtime.bigint() - app.started) / 1e6;
      }
    } catch (_) { /* not listening yet */ }
    await sleep(10);
  }
  throw new Error('instance did not answer ping within ' + timeoutMs + ' ms');
}

async function stop(app) {
  try { await post(app.port, 'quit', {}, 2000); } catch (_) { /* already closing */ }
  for (let i = 0; i < 40 && app.child.exitCode === null && app.child.signalCode === null; i++) await sleep(100);
  if (app.child.exitCode === null && app.child.signalCode === null) {
    // our own tree only: other instances and the user's real Shotik stay untouched
    try { execFileSync('taskkill', ['/F', '/T', '/PID', String(app.child.pid)], { stdio: 'ignore' }); } catch (_) { /* already gone */ }
  }
}

// Snapshot of a process tree: memory and accumulated CPU time.
function snapshot(rootPid) {
  const cmd = 'Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,HandleCount,PrivatePageCount,WorkingSetSize,KernelModeTime,UserModeTime | ConvertTo-Json -Compress';
  const all = JSON.parse(execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', cmd], { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 }));
  const ids = new Set([rootPid]);
  for (let grew = true; grew;) {
    grew = false;
    for (const p of all) {
      if (ids.has(p.ParentProcessId) && !ids.has(p.ProcessId)) { ids.add(p.ProcessId); grew = true; }
    }
  }
  const tree = all.filter((p) => ids.has(p.ProcessId));
  const root = tree.find((p) => p.ProcessId === rootPid) || { PrivatePageCount: 0, WorkingSetSize: 0 };
  const mb = (n) => Math.round(n / 1048576);
  return {
    at: Date.now(),
    procs: tree.length,
    rootPrivateMB: mb(root.PrivatePageCount),
    treePrivateMB: mb(tree.reduce((s, p) => s + p.PrivatePageCount, 0)),
    treeWorkingSetMB: mb(tree.reduce((s, p) => s + p.WorkingSetSize, 0)),
    treeHandles: tree.reduce((s, p) => s + Number(p.HandleCount || 0), 0),
    cpuMs: tree.reduce((s, p) => s + Number(p.KernelModeTime || 0) + Number(p.UserModeTime || 0), 0) / 10000,
  };
}

// CPU between two snapshots: CPU milliseconds per second (1000 = one core fully busy).
const cpuRate = (a, b) => Math.round(((b.cpuMs - a.cpuMs) / Math.max(1, b.at - a.at)) * 1000);

function stats(values) {
  const v = values.filter((x) => typeof x === 'number' && Number.isFinite(x)).sort((a, b) => a - b);
  if (!v.length) return null;
  const q = (p) => v[Math.min(v.length - 1, Math.floor(p * v.length))];
  return { n: v.length, min: Math.round(v[0]), median: Math.round(q(0.5)), p95: Math.round(q(0.95)), max: Math.round(v[v.length - 1]) };
}

function parseMarks(marks) {
  const out = {};
  for (const m of marks || []) {
    const [k, val] = String(m).split('=');
    out[k] = Number(val);
  }
  return out;
}

module.exports = { sleep, post, mcp, textOf, launch, waitReady, stop, snapshot, cpuRate, stats, parseMarks };
