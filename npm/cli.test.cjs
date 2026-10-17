'use strict';

const assert = require('node:assert/strict');
const { once } = require('node:events');
const { mkdirSync, mkdtempSync, rmSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');
const { spawn, spawnSync } = require('node:child_process');
const test = require('node:test');
const { version } = require('../package.json');

const cli = join(__dirname, 'cli.cjs');

function fixture(t, source = 'console.log(JSON.stringify(process.argv.slice(2)));') {
  const root = mkdtempSync(join(tmpdir(), 'hypergsr-cli-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const directory = join(root, 'native', 'linux-x64');
  mkdirSync(directory, { recursive: true });
  writeFileSync(join(directory, 'gpu-recorder-rs'), `#!${process.execPath}\n${source}\n`, { mode: 0o755 });
  writeFileSync(join(directory, 'hypergsr-pw-capture'), '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  return root;
}

function command(root, args, overrides = {}) {
  return ['-e', `require(${JSON.stringify(cli)}).main(${JSON.stringify({
    root, args, platform: 'linux', arch: 'x64', ...overrides,
  })})`];
}

function invoke(root, args, options = {}) {
  return spawnSync(process.execPath, command(root, args), {
    encoding: 'utf8',
    timeout: 10000,
    ...options,
  });
}

test('help and version work without native binaries', () => {
  for (const flag of ['--help', '-h']) {
    const result = invoke('/missing-hypergsr', [flag]);
    assert.equal(result.status, 0);
    assert.match(result.stdout, /Usage: hypergsr/);
    assert.match(result.stdout, /Linux x86-64/);
  }
  for (const flag of ['--version', '-v']) {
    const result = invoke('/missing-hypergsr', [flag]);
    assert.equal(result.status, 0);
    assert.equal(result.stdout.trim(), version);
  }
});

test('unsupported platforms and architectures fail clearly', () => {
  for (const [platform, arch] of [['darwin', 'arm64'], ['win32', 'x64'], ['linux', 'arm64']]) {
    const result = spawnSync(process.execPath, command('/missing', [], { platform, arch }), {
      encoding: 'utf8',
    });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /supports Linux x86-64/);
  }
});

test('missing binaries produce an actionable error', () => {
  const result = invoke('/missing-hypergsr', ['--report']);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Reinstall the npm package/);
  assert.match(result.stderr, /npm run build:native/);
});

test('launches the native app without arguments', (t) => {
  const root = fixture(t);
  const result = invoke(root, []);
  assert.equal(result.status, 0);
  assert.deepEqual(JSON.parse(result.stdout), []);
});

test('forwards arguments literally and preserves cwd and environment', (t) => {
  const root = fixture(t, `console.log(JSON.stringify({
    args: process.argv.slice(2), cwd: process.cwd(), theme: process.env.HYPERGSR_THEME
  }));`);
  const args = ['--export-demo', './a file; $(not-a-command).mp4', '"quoted"', '--', '*'];
  const result = invoke(root, args, {
    cwd: root,
    env: { ...process.env, HYPERGSR_THEME: 'light' },
  });
  assert.equal(result.status, 0);
  assert.deepEqual(JSON.parse(result.stdout), { args, cwd: root, theme: 'light' });
});

test('inherits stdin, stdout, and stderr', (t) => {
  const root = fixture(t, `
    process.stdout.write(require('node:fs').readFileSync(0));
    process.stderr.write('native diagnostic');
  `);
  const result = invoke(root, ['--record-seconds', '1'], { input: 'native input' });
  assert.equal(result.status, 0);
  assert.equal(result.stdout, 'native input');
  assert.equal(result.stderr, 'native diagnostic');
});

test('preserves a nonzero native exit code', (t) => {
  const result = invoke(fixture(t, 'process.exit(37);'), ['--report']);
  assert.equal(result.status, 37);
});

test('reports native spawn errors without an uncaught exception', (t) => {
  const root = fixture(t);
  writeFileSync(join(root, 'native', 'linux-x64', 'gpu-recorder-rs'), '#!/missing-interpreter\n');
  const result = invoke(root, ['--report']);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Could not start HyperGSR/);
  assert.doesNotMatch(result.stderr, /Unhandled 'error'/);
});

test('preserves termination by signal', (t) => {
  const result = invoke(fixture(t, "process.kill(process.pid, 'SIGTERM');"), ['--report']);
  assert.equal(result.signal, 'SIGTERM');
});

test('forwards SIGTERM to the native app', { timeout: 10000 }, async (t) => {
  const root = fixture(t, `
    process.on('SIGTERM', () => process.exit(23));
    console.log('ready');
    setInterval(() => {}, 1000);
  `);
  const child = spawn(process.execPath, command(root, ['--report']), {
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  t.after(() => {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
  });
  const exited = once(child, 'exit');
  await once(child.stdout, 'data');
  child.kill('SIGTERM');
  const [code, signal] = await exited;
  assert.equal(code, 23);
  assert.equal(signal, null);
});
