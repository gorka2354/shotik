'use strict';
// A web page in the user's browser must not reach the MCP server: not by POSTing
// to localhost, and not through DNS rebinding, where its own domain resolves to
// 127.0.0.1 and it can read the reply.
const net = require('net');
const path = require('path');
const { test, assert, eq } = require('../tiny');
const mcp = require(path.join(__dirname, '..', '..', 'src', 'main', 'mcp.js'));

test('only loopback hosts pass', () => {
  for (const host of ['127.0.0.1:7464', 'localhost:7464', 'LOCALHOST:7464', '[::1]:7464', '127.0.0.1', 'localhost']) {
    assert(mcp.isLocalRequest(host), host);
  }
  for (const host of ['evil.example:7464', 'evil.example', '127.0.0.1.evil.example', 'localhost.evil.example:7464', '', '[::2]:7464', undefined,
    '127.0.0.1:80@evil.example', 'localhost@evil.example', 'localhost:x', 'localhost:80:80', 'localhost.', '127.0.0.1, evil.example']) {
    assert(!mcp.isLocalRequest(host), String(host));
  }
});

test('an origin must be loopback too', () => {
  const host = '127.0.0.1:7464';
  for (const origin of ['http://localhost:3000', 'http://127.0.0.1:6274', 'https://localhost', 'http://[::1]:8080']) {
    assert(mcp.isLocalRequest(host, origin), origin);
  }
  for (const origin of ['http://evil.example', 'https://evil.example:7464', 'http://localhost.evil.example', 'http://127.0.0.1.evil.example', 'null', 'file://', 'chrome-extension://abc', '',
    'http://localhost:1@evil.example', 'http://localhost@evil.example', 'http://localhost/evil', 'http://localhost:6274, http://evil.example', 'http://localhost.']) {
    assert(!mcp.isLocalRequest(host, origin), origin);
  }
});

// A raw socket, because HTTP clients refuse to fake Host.
const statusOf = (port, headers) => new Promise((resolve, reject) => {
  const body = '{"jsonrpc":"2.0","id":1,"method":"tools/list"}';
  const socket = net.connect(port, '127.0.0.1', () => socket.write(
    `POST /mcp HTTP/1.1\r\n${headers}Content-Type: text/plain\r\nContent-Length: ${body.length}\r\nConnection: close\r\n\r\n${body}`));
  let reply = '';
  socket.on('data', (d) => { reply += d; });
  socket.on('end', () => resolve(Number(reply.split(' ')[1])));
  socket.on('error', reject);
});

test('the server refuses a page and serves a local client', async () => {
  const port = 7699;
  await mcp.start(port);
  try {
    eq(await statusOf(port, `Host: 127.0.0.1:${port}\r\n`), 200);
    eq(await statusOf(port, `Host: evil.example:${port}\r\nOrigin: http://evil.example:${port}\r\n`), 403);
    eq(await statusOf(port, `Host: 127.0.0.1:${port}\r\nOrigin: https://evil.example\r\n`), 403);
    eq(await statusOf(port, `Host: 127.0.0.1:${port}\r\nOrigin: http://localhost:1@evil.example\r\n`), 403);
    eq(await statusOf(port, `Host: 127.0.0.1:${port}\r\nOrigin: http://localhost:6274\r\nOrigin: http://evil.example\r\n`), 403);
    eq(await statusOf(port, 'Host: 127.0.0.1:80@evil.example\r\n'), 403);
  } finally { await mcp.stop(); }
});
