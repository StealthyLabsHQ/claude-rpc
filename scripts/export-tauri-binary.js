#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const ROOT = path.resolve(__dirname, '..');
const releaseDir = path.join(ROOT, 'src-tauri', 'target', 'release');
const targetDir = path.join(ROOT, 'bin');

function fail(message) {
  console.error(`[export-tauri] ${message}`);
  process.exit(1);
}

function firstExisting(paths) {
  return paths.find((candidate) => fs.existsSync(candidate));
}

function copyExecutable(source, target) {
  fs.rmSync(path.join(targetDir, 'runtime'), { recursive: true, force: true });
  fs.rmSync(path.join(targetDir, 'claude-rpc-daemon.exe'), { force: true });
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.copyFileSync(source, target);
  if (process.platform !== 'win32') {
    fs.chmodSync(target, 0o755);
  }
  console.log(`[export-tauri] ${path.relative(ROOT, target)}`);
}

if (process.platform === 'win32') {
  const source = firstExisting([
    path.join(releaseDir, 'claude-rpc.exe'),
    path.join(releaseDir, 'claude_rpc_tray.exe'),
  ]);
  if (!source) fail('missing Tauri release exe');
  copyExecutable(source, path.join(targetDir, 'claude-rpc.exe'));
} else if (process.platform === 'darwin') {
  const source = firstExisting([
    path.join(releaseDir, 'claude-rpc'),
    path.join(releaseDir, 'claude_rpc_tray'),
  ]);
  if (!source) fail('missing Tauri release binary');
  const arch = os.arch() === 'arm64' ? 'arm64' : 'x64';
  copyExecutable(source, path.join(targetDir, `claude-rpc-macos-${arch}`));
} else {
  const source = firstExisting([
    path.join(releaseDir, 'claude-rpc'),
    path.join(releaseDir, 'claude_rpc_tray'),
  ]);
  if (!source) fail(`unsupported platform ${process.platform}: missing Tauri release binary`);
  copyExecutable(source, path.join(targetDir, `claude-rpc-${process.platform}-${os.arch()}`));
}
