'use strict';

const assert = require('node:assert/strict');
const {
  chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync,
} = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');
const { spawnSync } = require('node:child_process');
const test = require('node:test');
const { checkPackage, icons } = require('./check-package.cjs');
const manifest = require('../package.json');

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'hypergsr-package-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  writeFileSync(join(root, 'package.json'), JSON.stringify(manifest));
  writeFileSync(join(root, 'Cargo.toml'), `[package]\nversion = "${manifest.version}"\n`);
  const directory = join(root, 'native', 'linux-x64');
  mkdirSync(join(directory, 'assets'), { recursive: true });
  const elf = Buffer.alloc(64);
  elf.write('7f454c46', 0, 'hex');
  elf[4] = 2;
  elf[5] = 1;
  elf.writeUInt16LE(62, 18);
  for (const name of ['gpu-recorder-rs', 'hypergsr-pw-capture']) {
    writeFileSync(join(directory, name), elf, { mode: 0o755 });
  }
  for (const name of icons) writeFileSync(join(directory, 'assets', name), '<svg/>');
  writeFileSync(join(directory, 'build.json'), JSON.stringify({
    version: manifest.version, target: 'x86_64-unknown-linux-gnu', glibc: '2.39',
  }));
  return root;
}

test('has no runtime dependencies or install-time scripts', () => {
  assert.equal(manifest.name, 'hypergsr');
  assert.deepEqual(manifest.bin, { hypergsr: 'npm/cli.cjs' });
  assert.deepEqual(manifest.os, ['linux']);
  assert.deepEqual(manifest.cpu, ['x64']);
  assert.deepEqual(manifest.libc, ['glibc']);
  assert.deepEqual(manifest.dependencies || {}, {});
  for (const hook of ['preinstall', 'install', 'postinstall', 'prepare']) {
    assert.equal(manifest.scripts[hook], undefined);
  }
});

test('accepts a complete, version-matched x86-64 bundle', (t) => {
  assert.doesNotThrow(() => checkPackage(fixture(t)));
});

test('rejects npm/Cargo version drift', (t) => {
  const root = fixture(t);
  writeFileSync(join(root, 'Cargo.toml'), '[package]\nversion = "0.0.0"\n');
  assert.throws(() => checkPackage(root), /versions in sync/);
});

test('rejects non-ELF and wrong-architecture binaries', (t) => {
  const root = fixture(t);
  const binary = join(root, 'native', 'linux-x64', 'gpu-recorder-rs');
  const elf = readFileSync(binary);
  elf.writeUInt16LE(183, 18);
  writeFileSync(binary, elf);
  assert.throws(() => checkPackage(root), /x86-64 ELF/);
  writeFileSync(binary, 'not an ELF binary');
  assert.throws(() => checkPackage(root), /x86-64 ELF/);
});

test('rejects non-executable binaries', (t) => {
  const root = fixture(t);
  chmodSync(join(root, 'native', 'linux-x64', 'hypergsr-pw-capture'), 0o644);
  assert.throws(() => checkPackage(root), /EACCES/);
});

test('rejects missing icons and stale build metadata', (t) => {
  const root = fixture(t);
  const directory = join(root, 'native', 'linux-x64');
  writeFileSync(join(directory, 'build.json'), JSON.stringify({ version: '0.0.0' }));
  assert.throws(() => checkPackage(root), /metadata does not match/);
  writeFileSync(join(directory, 'build.json'), JSON.stringify({
    version: manifest.version, target: 'x86_64-unknown-linux-gnu',
  }));
  rmSync(join(directory, 'assets', icons[0]));
  assert.throws(() => checkPackage(root), /ENOENT/);
});

test('tarball includes only the launcher, runtime bundle, and public metadata', (t) => {
  const root = fixture(t);
  mkdirSync(join(root, 'npm'));
  copyFileSync(join(__dirname, 'cli.cjs'), join(root, 'npm', 'cli.cjs'));
  for (const name of ['README.md', 'LICENSE']) {
    copyFileSync(join(__dirname, '..', name), join(root, name));
  }
  writeFileSync(join(root, 'private-notes.txt'), 'must not ship');
  writeFileSync(join(root, 'npm', 'build-native.cjs'), 'must not ship');
  const result = spawnSync('npm', ['pack', '--dry-run', '--ignore-scripts', '--json'], {
    cwd: root, encoding: 'utf8', timeout: 30000,
  });
  assert.equal(result.status, 0, result.stderr);
  const [pack] = JSON.parse(result.stdout);
  const paths = pack.files.map((file) => file.path).sort();
  assert.deepEqual(paths, [
    'LICENSE', 'README.md', 'package.json', 'npm/cli.cjs',
    'native/linux-x64/gpu-recorder-rs',
    'native/linux-x64/hypergsr-pw-capture',
    'native/linux-x64/build.json',
    ...icons.map((name) => `native/linux-x64/assets/${name}`),
  ].sort());
});
