#!/usr/bin/env node
'use strict';

const {
  accessSync, constants, existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync,
} = require('node:fs');
const { homedir } = require('node:os');
const { dirname, isAbsolute, join } = require('node:path');
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
  --install-desktop         Add HyperGSR to your app menu (current user only)
  --uninstall-desktop       Remove the app menu entry and icon

Requires Linux x86-64 with glibc, GTK >= 4.10, gtk4-layer-shell,
libdrm, X11 libraries, PipeWire, libportal, FFmpeg, and a desktop portal.
No Rust toolchain, sudo, or install-time downloads are used by this package.
`;

const appId = 'io.github.KunalSiyag.gpu_recorder_rs';

function dataHome(env) {
  if (env.XDG_DATA_HOME && isAbsolute(env.XDG_DATA_HOME)) return env.XDG_DATA_HOME;
  return join(env.HOME || homedir(), '.local', 'share');
}

function desktopPaths(env) {
  const data = dataHome(env);
  return {
    entry: join(data, 'applications', `${appId}.desktop`),
    icon: join(data, 'icons', 'hicolor', 'scalable', 'apps', `${appId}.svg`),
  };
}

// Desktop Entry spec: quote Exec arguments with reserved characters, then escape
// backslashes once more because Exec is also a string value.
function execArgument(path) {
  if (/^[A-Za-z0-9_@+=:,./-]+$/.test(path)) return path;
  const quoted = `"${path.replace(/["`$\\]/g, '\\$&')}"`;
  return quoted.replace(/\\/g, '\\\\').replace(/%/g, '%%');
}

function desktopEntry(binary) {
  return `[Desktop Entry]
Type=Application
Name=HyperGSR
GenericName=Screen Recorder
Comment=GPU screen recorder with cursor-following auto-zoom
Exec=${execArgument(binary)}
Icon=${appId}
Terminal=false
StartupNotify=true
StartupWMClass=${appId}
Categories=AudioVideo;Recorder;Video;
Keywords=screencast;screen;recorder;wayland;
`;
}

function writeAtomic(file, contents, mode) {
  mkdirSync(dirname(file), { recursive: true });
  const temporary = `${file}.${process.pid}.tmp`;
  writeFileSync(temporary, contents, { mode });
  renameSync(temporary, file);
}

function installDesktop(directory, env) {
  const binary = join(directory, 'gpu-recorder-rs');
  const { entry, icon } = desktopPaths(env);
  writeAtomic(icon, readFileSync(join(directory, 'assets', 'hypergsr-logo.svg')), 0o644);
  writeAtomic(entry, desktopEntry(binary), 0o644);
  console.log(`Added HyperGSR to your app menu:\n  ${entry}\n  ${icon}`);
  console.log('The entry runs this installed copy. Run --install-desktop again after');
  console.log('reinstalling HyperGSR under a different Node.js version or prefix.');
}

function uninstallDesktop(env) {
  const { entry, icon } = desktopPaths(env);
  let removed = false;
  for (const file of [entry, icon]) {
    if (existsSync(file)) {
      rmSync(file, { force: true });
      console.log(`Removed ${file}`);
      removed = true;
    }
  }
  if (!removed) console.log('No HyperGSR app menu entry was installed.');
}
function main({
  args = process.argv.slice(2),
  platform = process.platform,
  arch = process.arch,
  root = join(__dirname, '..'),
  env = process.env,
} = {}) {
  if (args.length === 1 && ['--help', '-h'].includes(args[0])) {
    console.log(help);
    return;
  }
  if (args.length === 1 && ['--version', '-v'].includes(args[0])) {
    console.log(version);
    return;
  }
  if (args.length === 1 && args[0] === '--uninstall-desktop') {
    uninstallDesktop(env);
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

  if (args.length === 1 && args[0] === '--install-desktop') {
    try {
      installDesktop(directory, env);
    } catch (error) {
      console.error(`Could not add the app menu entry: ${error.message}`);
      process.exitCode = 1;
    }
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
module.exports = { appId, desktopEntry, execArgument, main };
