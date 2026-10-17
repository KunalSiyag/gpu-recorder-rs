'use strict';

const {
  chmodSync, copyFileSync, mkdirSync, mkdtempSync, renameSync, rmSync, writeFileSync,
} = require('node:fs');
const { join } = require('node:path');
const { spawnSync } = require('node:child_process');
const { version } = require('../package.json');
const { checkPackage, icons } = require('./check-package.cjs');

const root = join(__dirname, '..');
const target = 'x86_64-unknown-linux-gnu';

function run(command, args, capture = false) {
  const result = spawnSync(command, args, {
    cwd: root,
    stdio: capture ? ['ignore', 'pipe', 'inherit'] : 'inherit',
    encoding: 'utf8',
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed (${result.signal || result.status}).`);
  return result.stdout?.trim();
}

let staging;
try {
  const glibc = process.report.getReport().header.glibcVersionRuntime;
  if (process.platform !== 'linux' || process.arch !== 'x64' || !glibc) {
    throw new Error('Build the npm package on Linux x86-64 with glibc (CI uses Ubuntu 24.04).');
  }
  run('cargo', [
    'build', '--release', '--locked', '--target', target,
    '--target-dir', join(root, 'target'),
  ]);
  mkdirSync(join(root, 'native'), { recursive: true });
  staging = mkdtempSync(join(root, 'native', '.build-'));
  const flags = run('pkg-config', ['--cflags', '--libs', 'libpipewire-0.3', 'gio-2.0'], true)
    .split(/\s+/).filter(Boolean);
  // The helper declares libportal's ABI directly and needs no libportal headers.
  run('gcc', [
    '-O2', '-Wall', join(root, 'src', 'pw_portal_capture.c'),
    '-o', join(staging, 'hypergsr-pw-capture'),
    ...flags, '-l:libportal.so.1', '-lpthread',
  ]);
  copyFileSync(join(root, 'target', target, 'release', 'gpu-recorder-rs'), join(staging, 'gpu-recorder-rs'));
  for (const name of ['gpu-recorder-rs', 'hypergsr-pw-capture']) {
    chmodSync(join(staging, name), 0o755);
  }
  mkdirSync(join(staging, 'assets'));
  for (const name of icons) {
    copyFileSync(join(root, 'assets', name), join(staging, 'assets', name));
  }
  writeFileSync(join(staging, 'build.json'), `${JSON.stringify({ version, target, glibc }, null, 2)}\n`);
  const destination = join(root, 'native', 'linux-x64');
  rmSync(destination, { recursive: true, force: true });
  renameSync(staging, destination);
  staging = undefined;
  checkPackage(root);
  console.log(`Built hypergsr ${version} for ${target} (build host glibc ${glibc}).`);
} catch (error) {
  console.error(error.message);
  console.error('See the README for native build dependencies.');
  process.exitCode = 1;
} finally {
  if (staging) rmSync(staging, { recursive: true, force: true });
}
