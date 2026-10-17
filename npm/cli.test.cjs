'use strict';

const assert = require('node:assert/strict');
const { once } = require('node:events');
const {
  existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync,
} = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');
const { spawn, spawnSync } = require('node:child_process');
const test = require('node:test');
const { version } = require('../package.json');
const { appId, desktopEntry, execArgument } = require('./cli.cjs');

const cli = join(__dirname, 'cli.cjs');

function fixture(t, source = 'console.log(JSON.stringify(process.argv.slice(2)));') {
  const root = mkdtempSync(join(tmpdir(), 'hypergsr-cli-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const directory = join(root, 'native', 'linux-x64');
  mkdirSync(directory, { recursive: true });
  writeFileSync(join(directory, 'gpu-recorder-rs'), `#!${process.execPath}\n${source}\n`, { mode: 0o755 });
  writeFileSync(join(directory, 'hypergsr-pw-capture'), '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  mkdirSync(join(directory, 'assets'));
  writeFileSync(join(directory, 'assets', 'hypergsr-logo.svg'), '<svg/>');
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

test('--install-desktop adds a menu entry and icon for the installed binary', (t) => {
  const root = fixture(t, 'console.log("native app must not start"); process.exit(9);');
  const data = join(root, 'data home');
  const result = spawnSync(process.execPath, command(root, ['--install-desktop'], {
    env: { XDG_DATA_HOME: data, HOME: root },
  }), { encoding: 'utf8', timeout: 10000 });
  assert.equal(result.status, 0, result.stderr);
  assert.doesNotMatch(result.stdout, /must not start/);
  const entry = readFileSync(join(data, 'applications', `${appId}.desktop`), 'utf8');
  const binary = join(root, 'native', 'linux-x64', 'gpu-recorder-rs');
  assert.match(entry, /^\[Desktop Entry\]\nType=Application\n/);
  assert.ok(entry.includes(`Exec=${execArgument(binary)}\n`));
  assert.match(entry, new RegExp(`^Icon=${appId.replace(/\./g, '\\.')}$`, 'm'));
  assert.match(entry, new RegExp(`^StartupWMClass=${appId.replace(/\./g, '\\.')}$`, 'm'));
  const icon = join(data, 'icons', 'hicolor', 'scalable', 'apps', `${appId}.svg`);
  assert.equal(readFileSync(icon, 'utf8'), '<svg/>');

  const validator = spawnSync('desktop-file-validate', [join(data, 'applications', `${appId}.desktop`)], {
    encoding: 'utf8',
  });
  if (!validator.error) assert.equal(validator.status, 0, validator.stdout + validator.stderr);

  const removed = spawnSync(process.execPath, command(root, ['--uninstall-desktop'], {
    env: { XDG_DATA_HOME: data, HOME: root },
  }), { encoding: 'utf8', timeout: 10000 });
  assert.equal(removed.status, 0, removed.stderr);
  assert.equal(existsSync(join(data, 'applications', `${appId}.desktop`)), false);
  assert.equal(existsSync(icon), false);
  const again = spawnSync(process.execPath, command(root, ['--uninstall-desktop'], {
    env: { XDG_DATA_HOME: data, HOME: root },
  }), { encoding: 'utf8', timeout: 10000 });
  assert.equal(again.status, 0);
  assert.match(again.stdout, /No HyperGSR app menu entry/);
});

test('--install-desktop falls back to ~/.local/share and needs the native bundle', (t) => {
  const root = fixture(t);
  const result = spawnSync(process.execPath, command(root, ['--install-desktop'], {
    env: { HOME: root, XDG_DATA_HOME: 'relative/ignored' },
  }), { encoding: 'utf8', timeout: 10000 });
  assert.equal(result.status, 0, result.stderr);
  assert.ok(existsSync(join(root, '.local', 'share', 'applications', `${appId}.desktop`)));

  const missing = invoke('/missing-hypergsr', ['--install-desktop']);
  assert.equal(missing.status, 1);
  assert.match(missing.stderr, /Reinstall the npm package/);
});

test('Exec paths with spaces and reserved characters are quoted', () => {
  assert.equal(execArgument('/usr/lib/node_modules/hypergsr/native/linux-x64/gpu-recorder-rs'),
    '/usr/lib/node_modules/hypergsr/native/linux-x64/gpu-recorder-rs');
  assert.equal(execArgument('/home/me/My Apps/gpu-recorder-rs'), '"/home/me/My Apps/gpu-recorder-rs"');
  assert.equal(execArgument('/a"b$c`d\\e'), '"/a\\\\"b\\\\$c\\\\`d\\\\\\\\e"');
  assert.equal(execArgument('/100%/x'), '"/100%%/x"');
  assert.match(desktopEntry('/opt/hypergsr'), /^Exec=\/opt\/hypergsr$/m);
});
