'use strict';

const { accessSync, constants, readFileSync } = require('node:fs');
const { join } = require('node:path');

const icons = [
  'hypergsr-logo.svg',
  'icon-autozoom.svg',
  'icon-gpu.svg',
  'icon-record.svg',
  'icon-stop.svg',
  'icon-sun.svg',
  'icon-theme.svg',
];

function checkPackage(root = join(__dirname, '..')) {
  const { version } = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
  const cargo = readFileSync(join(root, 'Cargo.toml'), 'utf8');
  if (cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1] !== version) {
    throw new Error('Keep package.json and Cargo.toml versions in sync before packing.');
  }
  const directory = join(root, 'native', 'linux-x64');
  for (const name of ['gpu-recorder-rs', 'hypergsr-pw-capture']) {
    const file = join(directory, name);
    accessSync(file, constants.X_OK);
    const header = readFileSync(file).subarray(0, 20);
    if (
      header.length !== 20 ||
      header.subarray(0, 4).toString('hex') !== '7f454c46' ||
      header[4] !== 2 ||
      header[5] !== 1 ||
      header.readUInt16LE(18) !== 62
    ) {
      throw new Error(`${name} must be a Linux x86-64 ELF binary.`);
    }
  }
  for (const name of icons) {
    accessSync(join(directory, 'assets', name), constants.R_OK);
  }
  const build = JSON.parse(readFileSync(join(directory, 'build.json'), 'utf8'));
  if (build.version !== version || build.target !== 'x86_64-unknown-linux-gnu') {
    throw new Error('Native build metadata does not match this package.');
  }
}

if (require.main === module) {
  try {
    checkPackage();
    console.log('Native npm package contents verified.');
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
module.exports = { checkPackage, icons };
