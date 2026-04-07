#!/usr/bin/env node
'use strict';

/**
 * build-dist.js — Build dist/ matching the release architecture:
 *   dist/
 *     claude-rpc.exe    ← PyInstaller GUI exe (no console)
 *     runtime/
 *       node.exe        ← Real Node.js (windowsHide works)
 *       tray.js, index.js, secure-env.js, package.json
 *       node_modules/
 *     logo/
 */

const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');

const ROOT = path.resolve(__dirname, '..');
const DIST = path.join(ROOT, 'dist');
const RUNTIME = path.join(DIST, 'runtime');
const LOGO_DIST = path.join(DIST, 'logo');

// --- Clean & create ---
console.log('Cleaning dist/...');
fs.rmSync(DIST, { recursive: true, force: true });
fs.mkdirSync(RUNTIME, { recursive: true });
fs.mkdirSync(LOGO_DIST, { recursive: true });

// --- Build claude-rpc.exe via PyInstaller ---
console.log('Building claude-rpc.exe (PyInstaller)...');
execSync([
  'pyinstaller',
  '--onefile',
  '--windowed',
  '--icon=logo/anthropic-rpc.ico',
  '--name=claude-rpc',
  '--distpath=dist',
  '--version-file=version_info.txt',
  'main.py',
].join(' '), { cwd: ROOT, stdio: 'inherit' });

// --- Copy runtime files ---
console.log('Copying runtime files...');
for (const f of ['tray.js', 'index.js', 'secure-env.js', 'package.json']) {
  fs.copyFileSync(path.join(ROOT, f), path.join(RUNTIME, f));
}

// --- Copy .env if exists ---
const envFile = path.join(ROOT, '.env');
if (fs.existsSync(envFile)) {
  fs.copyFileSync(envFile, path.join(RUNTIME, '.env'));
}

// --- Install production deps in runtime ---
console.log('Installing production dependencies...');
execSync('npm install --omit=dev --ignore-scripts --no-fund --no-audit', {
  cwd: RUNTIME,
  stdio: 'inherit',
});

// --- Copy node.exe ---
console.log('Bundling node.exe...');
fs.copyFileSync(process.execPath, path.join(RUNTIME, 'node.exe'));

// --- Copy logo ---
console.log('Copying logo assets...');
const logoSrc = path.join(ROOT, 'logo');
if (fs.existsSync(logoSrc)) {
  for (const f of fs.readdirSync(logoSrc)) {
    fs.copyFileSync(path.join(logoSrc, f), path.join(LOGO_DIST, f));
  }
}

console.log('');
console.log('Build complete!');
console.log('  dist/claude-rpc.exe  ← Double-click to launch (silent, tray only)');
console.log('  dist/runtime/        ← node.exe + JS + node_modules');
console.log('  dist/logo/           ← Icons');
