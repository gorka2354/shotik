'use strict';
// Built-in MCP server (Streamable HTTP, stateless) — lets Claude Code & other MCP
// clients take screenshots of the user's screen on demand. Zero dependencies.
//
//   claude mcp add shotik --transport http http://127.0.0.1:7464/mcp
//
const http = require('http');
const crypto = require('crypto');

const VERSION = require('../../package.json').version;
const PROTOCOL_FALLBACK = '2025-03-26';

let server = null;
let port = null;
const log = []; // ring buffer of recent tool calls
const LOG_MAX = 60;

let toolRegistry = {}; // name -> { description, inputSchema, handler(args) -> content[] }
let testHandler = null; // (route, body) -> Promise<any> | null
let onLogUpdate = () => {};

function setTools(tools) { toolRegistry = tools; }
function setTestHandler(fn) { testHandler = fn; }
function setLogListener(fn) { onLogUpdate = fn; }
function getLog() { return [...log]; }
function status() { return { running: !!server, port }; }

function pushLog(entry) {
  log.unshift(entry);
  if (log.length > LOG_MAX) log.pop();
  try { onLogUpdate(entry); } catch (_) {}
}

function jsonRes(res, code, obj, extraHeaders = {}) {
  const body = obj === null ? '' : JSON.stringify(obj);
  res.writeHead(code, { 'Content-Type': 'application/json', ...extraHeaders });
  res.end(body);
}

function rpcResult(id, result) { return { jsonrpc: '2.0', id, result }; }
function rpcError(id, code, message) { return { jsonrpc: '2.0', id, error: { code, message } }; }

async function handleRpc(msg) {
  const { method, id, params } = msg;
  if (method === 'initialize') {
    return rpcResult(id, {
      protocolVersion: (params && params.protocolVersion) || PROTOCOL_FALLBACK,
      capabilities: { tools: { listChanged: false } },
      serverInfo: { name: 'shotik', title: 'Shotik Screenshots', version: VERSION },
      instructions: 'Screenshot tools for the user\'s screen. Use take_screenshot to see the screen, ask_user_to_select_region to let the user mark a specific area.',
    });
  }
  if (method === 'ping') return rpcResult(id, {});
  if (method === 'tools/list') {
    return rpcResult(id, {
      tools: Object.entries(toolRegistry).map(([name, t]) => ({
        name, description: t.description, inputSchema: t.inputSchema,
      })),
    });
  }
  if (method === 'tools/call') {
    const name = params && params.name;
    const tool = toolRegistry[name];
    if (!tool) return rpcError(id, -32602, `Unknown tool: ${name}`);
    const started = Date.now();
    try {
      const content = await tool.handler((params && params.arguments) || {});
      pushLog({ ts: Date.now(), tool: name, args: (params && params.arguments) || {}, ms: Date.now() - started, ok: true });
      return rpcResult(id, { content, isError: false });
    } catch (e) {
      pushLog({ ts: Date.now(), tool: name, args: (params && params.arguments) || {}, ms: Date.now() - started, ok: false, error: String(e.message || e) });
      return rpcResult(id, { content: [{ type: 'text', text: 'Error: ' + (e.message || e) }], isError: true });
    }
  }
  if (id === undefined || id === null) return null; // notification — no response
  return rpcError(id, -32601, `Method not found: ${method}`);
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    req.on('data', (c) => {
      size += c.length;
      if (size > 64 * 1024 * 1024) { reject(new Error('body too large')); req.destroy(); return; }
      chunks.push(c);
    });
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });
}

async function requestListener(req, res) {
  const url = new URL(req.url, 'http://127.0.0.1');

  // ---- test endpoints (only active when a testHandler is installed) ----
  if (testHandler && url.pathname.startsWith('/test/')) {
    try {
      const body = req.method === 'POST' ? JSON.parse((await readBody(req)) || '{}') : {};
      const out = await testHandler(url.pathname.slice(6), body);
      return jsonRes(res, 200, { ok: true, result: out === undefined ? null : out });
    } catch (e) {
      return jsonRes(res, 500, { ok: false, error: String(e.message || e) });
    }
  }

  if (url.pathname !== '/mcp') return jsonRes(res, 404, { error: 'not found' });

  if (req.method === 'GET') {
    // We never push server-initiated messages; tell the client SSE is unsupported.
    res.writeHead(405, { Allow: 'POST, DELETE' });
    return res.end();
  }
  if (req.method === 'DELETE') { res.writeHead(200); return res.end(); }
  if (req.method !== 'POST') { res.writeHead(405, { Allow: 'POST, DELETE' }); return res.end(); }

  let parsed;
  try { parsed = JSON.parse(await readBody(req)); }
  catch (_) { return jsonRes(res, 400, rpcError(null, -32700, 'Parse error')); }

  const isInit = !Array.isArray(parsed) && parsed.method === 'initialize';
  const headers = isInit ? { 'Mcp-Session-Id': crypto.randomUUID() } : {};

  if (Array.isArray(parsed)) {
    const answers = (await Promise.all(parsed.map(handleRpc))).filter(Boolean);
    if (!answers.length) { res.writeHead(202, headers); return res.end(); }
    return jsonRes(res, 200, answers, headers);
  }
  const answer = await handleRpc(parsed);
  if (!answer) { res.writeHead(202, headers); return res.end(); }
  return jsonRes(res, 200, answer, headers);
}

function start(p) {
  return new Promise((resolve, reject) => {
    if (server) { resolve(port); return; }
    const s = http.createServer((req, res) => {
      requestListener(req, res).catch((e) => {
        try { jsonRes(res, 500, { error: String(e.message || e) }); } catch (_) {}
      });
    });
    s.on('error', (e) => { server = null; reject(e); });
    s.listen(p, '127.0.0.1', () => { server = s; port = p; resolve(p); });
  });
}

function stop() {
  return new Promise((resolve) => {
    if (!server) return resolve();
    server.close(() => resolve());
    server = null; port = null;
  });
}

module.exports = { start, stop, status, setTools, setTestHandler, setLogListener, getLog };
