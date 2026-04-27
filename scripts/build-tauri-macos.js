#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const ROOT = path.resolve(__dirname, '..');
const pkg = require(path.join(ROOT, 'package.json'));
const TAURI = path.join(ROOT, 'node_modules', '.bin', 'tauri');
const APP_BUNDLE = path.join(
  ROOT,
  'src-tauri',
  'target',
  'release',
  'bundle',
  'macos',
  'Claude RPC.app',
);
const DMG_DIR = path.join(ROOT, 'src-tauri', 'target', 'release', 'bundle', 'dmg');
const DMG_NAME = `Claude RPC_${pkg.version}_${os.arch() === 'arm64' ? 'aarch64' : 'x64'}.dmg`;
const DMG_PATH = path.join(DMG_DIR, DMG_NAME);
const DMG_STAGING = path.join(ROOT, 'src-tauri', 'target', 'release', 'bundle', 'macos-dmg-staging');

function run(command, args) {
  const res = spawnSync(command, args, {
    cwd: ROOT,
    stdio: 'inherit',
    shell: false,
  });
  if (res.status !== 0) process.exit(res.status ?? 1);
}

run(TAURI, ['build', '--bundles', 'app']);
run('codesign', [
  '--force',
  '--deep',
  '--sign',
  process.env.APPLE_SIGNING_IDENTITY || '-',
  APP_BUNDLE,
]);
run('codesign', ['--verify', '--deep', '--strict', '--verbose=2', APP_BUNDLE]);
fs.rmSync(DMG_STAGING, { recursive: true, force: true });
fs.mkdirSync(DMG_STAGING, { recursive: true });
fs.cpSync(APP_BUNDLE, path.join(DMG_STAGING, 'Claude RPC.app'), { recursive: true });
try {
  fs.symlinkSync('/Applications', path.join(DMG_STAGING, 'Applications'));
} catch {
  // The symlink is a Finder convenience; the DMG remains usable without it.
}
fs.mkdirSync(DMG_DIR, { recursive: true });
run('hdiutil', [
  'create',
  '-volname',
  'Claude RPC',
  '-srcfolder',
  DMG_STAGING,
  '-ov',
  '-format',
  'UDZO',
  DMG_PATH,
]);
fs.rmSync(DMG_STAGING, { recursive: true, force: true });
run(process.execPath, [path.join(ROOT, 'scripts', 'export-tauri-binary.js')]);
