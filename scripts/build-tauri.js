#!/usr/bin/env node
'use strict';

const path = require('node:path');
const { spawnSync } = require('node:child_process');

const ROOT = path.resolve(__dirname, '..');

function run(command, args) {
  const res = spawnSync(command, args, {
    cwd: ROOT,
    stdio: 'inherit',
    shell: process.platform === 'win32',
  });
  if (res.error) {
    console.error(`Failed to run ${command}: ${res.error.message}`);
  }
  if (res.status !== 0) process.exit(res.status ?? 1);
}

function tauriBin() {
  return path.join(
    ROOT,
    'node_modules',
    '.bin',
    process.platform === 'win32' ? 'tauri.cmd' : 'tauri',
  );
}

if (process.platform === 'darwin') {
  require('./build-tauri-macos');
} else if (process.platform === 'win32') {
  run(tauriBin(), ['build', '--bundles', 'nsis']);
  run(process.execPath, [path.join(ROOT, 'scripts', 'export-tauri-binary.js')]);
} else {
  run(tauriBin(), ['build']);
  run(process.execPath, [path.join(ROOT, 'scripts', 'export-tauri-binary.js')]);
}
