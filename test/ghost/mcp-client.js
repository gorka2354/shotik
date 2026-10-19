'use strict';
// Minimal MCP client for ghost tests: one JSON-RPC tools/call to a ghost instance.
const http = require('http');

// raw MCP JSON-RPC call to the ghost instance
function mcpCall(port, name, args) {
  return new Promise((resolve, reject) => {
    const data = Buffer.from(JSON.stringify({
      jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args || {} },
    }));
    const req = http.request({ host: '127.0.0.1', port, path: '/mcp', method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Content-Length': data.length } },
      (res) => { let b = ''; res.on('data', (c) => (b += c)); res.on('end', () => {
        try { resolve(JSON.parse(b).result); } catch (e) { reject(new Error('bad rpc reply: ' + b.slice(0, 200))); } }); });
    req.on('error', reject); req.write(data); req.end();
  });
}

const imgOf = (res) => (res.content || []).find((c) => c.type === 'image');
const textOf = (res) => ((res.content || []).find((c) => c.type === 'text') || {}).text || '';

module.exports = { mcpCall, imgOf, textOf };
