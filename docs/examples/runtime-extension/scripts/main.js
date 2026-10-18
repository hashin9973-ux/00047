'use strict';

const extensionId = process.env.EXTENSION_ID || 'local-example';
const extensionDir = process.env.EXTENSION_DIR || process.cwd();

console.log(`[${extensionId}] started from ${extensionDir}`);

let tick = 0;
const timer = setInterval(() => {
  tick += 1;
  console.log(`[${extensionId}] heartbeat ${tick}`);
}, 5000);

function shutdown(signal) {
  clearInterval(timer);
  console.log(`[${extensionId}] received ${signal}; exiting`);
  process.exit(0);
}

process.on('SIGINT', () => shutdown('SIGINT'));
process.on('SIGTERM', () => shutdown('SIGTERM'));
