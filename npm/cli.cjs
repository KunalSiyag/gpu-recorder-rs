#!/usr/bin/env node
'use strict';

const { accessSync, constants } = require('node:fs');
const { join } = require('node:path');
const { spawn } = require('node:child_process');
const { version } = require('../package.json');

const help = `HyperGSR ${version} — native Linux screen recorder

Usage: hypergsr [options]

  (no options)               Open the GTK4 Dynamic Island
  --web                     Serve Web Studio at http://127.0.0.1:7878
  --report                  Print GPU and encoder information as JSON
  --record-seconds <secs>    Record the desktop for a fixed duration
  --export-demo [output.mp4] Render an auto-zoom showcase
  --help, -h                Show this help
  --version, -v             Print the package version

Requires Linux x86-64 with glibc, GTK >= 4.10, gtk4-layer-shell,
libdrm, X11 libraries, PipeWire, libportal, FFmpeg, and a desktop portal.
No Rust toolchain, sudo, or install-time downloads are used by this package.
`;

function main({
  args = process.argv.slice(2),
  platform = process.platform,
  arch = process.arch,
  root = join(__dirname, '..'),
} = {}) {
  if (args.length === 1 && ['--help', '-h'].includes(args[0])) {
    console.log(help);
    return;
  }
  if (args.length === 1 && ['--version', '-v'].includes(args[0])) {
    console.log(version);
    return;
  }
  if (platform !== 'linux' || arch !== 'x64') {
    console.error(`HyperGSR supports Linux x86-64 (glibc), not ${platform}/${arch}.`);
    process.exitCode = 1;
    return;
  }

  const directory = join(root, 'native', 'linux-x64');
  try {
    for (const name of ['gpu-recorder-rs', 'hypergsr-pw-capture']) {
      accessSync(join(directory, name), constants.X_OK);
    }
  } catch {
    console.error('HyperGSR native binaries are missing or not executable. Reinstall the npm package.');
    console.error('From a source checkout, run npm run build:native first.');
    process.exitCode = 1;
    return;
  }

  // Keep the caller's cwd and environment, including relative export paths.
  const child = spawn(join(directory, 'gpu-recorder-rs'), args, {
    stdio: 'inherit',
    shell: false,
  });
  const handlers = new Map();
  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    const handler = () => child.kill(signal);
    handlers.set(signal, handler);
    process.on(signal, handler);
  }
  const cleanup = () => {
    for (const [signal, handler] of handlers) {
      process.removeListener(signal, handler);
    }
  };
  child.once('error', (error) => {
    cleanup();
    console.error(`Could not start HyperGSR: ${error.message}`);
    console.error('Check the Linux runtime libraries listed in the README.');
    process.exitCode = 1;
  });
  child.once('exit', (code, signal) => {
    cleanup();
    if (signal) {
      process.kill(process.pid, signal);
    } else {
      process.exitCode = code ?? 1;
    }
  });
  return child;
}

if (require.main === module) main();
module.exports = { main };
