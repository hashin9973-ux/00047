'use strict';

const readline = require('node:readline');

const extensionId = process.env.EXTENSION_ID || 'local-example';
const extensionDir = process.env.EXTENSION_DIR || process.cwd();

console.log(`[${extensionId}] started from ${extensionDir}`);

let tick = 0;
let stopping = false;
const timer = setInterval(() => {
  tick += 1;
  console.log(`[${extensionId}] heartbeat ${tick}`);
}, 5000);

function shutdown(reason) {
  if (stopping) return;
  stopping = true;
  clearInterval(timer);
  console.log(`[${extensionId}] stopping: ${reason}`);
  process.exit(0);
}

const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
input.on('line', line => {
  try {
    const message = JSON.parse(line);
    if (message.protocolVersion !== 1) return;
    if (message.type === 'host.started') {
      console.log(`[${extensionId}] host control channel ready`);
    } else if (message.type === 'host.shutdown') {
      shutdown('host.shutdown');
    }
  } catch (error) {
    console.error(`[${extensionId}] invalid control message: ${error.message}`);
  }
});

input.on('close', () => shutdown('stdin closed'));
process.on('SIGINT', () => shutdown('SIGINT'));
process.on('SIGTERM', () => shutdown('SIGTERM'));
