'use strict';
// Evaluate only the registration function: no Electron, app startup or tool calls.
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(process.argv[2], 'utf8');
const start = source.indexOf('function registerMcpTools() {');
const end = source.indexOf('\nasync function startMcp()', start);
if (start < 0 || end < 0) throw new Error('registerMcpTools not found in reference');
let definitions;
const mcp = {
  setTools(tools) {
    definitions = Object.entries(tools).map(([name, t]) => ({
      name, description: t.description, inputSchema: t.inputSchema,
    }));
  },
  setLogListener() {},
};
vm.runInNewContext(source.slice(start, end) + '\nregisterMcpTools();', { mcp });
const json = JSON.stringify(definitions, null, 2) + '\n';
if (process.argv[3]) fs.writeFileSync(process.argv[3], json, 'utf8');
else process.stdout.write(json);
