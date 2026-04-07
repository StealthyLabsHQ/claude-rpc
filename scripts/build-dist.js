#!/usr/bin/env node
'use strict';

/**
 * build-dist.js — Build a single claude-rpc.exe (all-in-one)
 *
 * Embeds runtime/ (node.exe + JS + node_modules) and logo/ inside
 * a PyInstaller --onefile --windowed exe. Zero console on launch.
 *
 * Output: dist/claude-rpc.exe (~47 MB, self-extracting)
 */

const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');

const ROOT = path.resolve(__dirname, '..');
const STAGING = path.join(ROOT, 'build', 'staging');
const RUNTIME = path.join(STAGING, 'runtime');

// --- Clean staging ---
console.log('Preparing staging area...');
fs.rmSync(STAGING, { recursive: true, force: true });
fs.mkdirSync(RUNTIME, { recursive: true });

// --- Copy runtime JS files ---
console.log('Copying runtime files...');
for (const f of ['tray.js', 'index.js', 'secure-env.js', 'package.json']) {
  fs.copyFileSync(path.join(ROOT, f), path.join(RUNTIME, f));
}
const envFile = path.join(ROOT, '.env');
if (fs.existsSync(envFile)) {
  fs.copyFileSync(envFile, path.join(RUNTIME, '.env'));
}

// --- Install production deps ---
console.log('Installing production dependencies...');
execSync('npm install --omit=dev --ignore-scripts --no-fund --no-audit', {
  cwd: RUNTIME,
  stdio: 'inherit',
});

// --- Copy node.exe ---
console.log('Bundling node.exe...');
fs.copyFileSync(process.execPath, path.join(RUNTIME, 'node.exe'));

// --- Copy logo ---
const logoDest = path.join(STAGING, 'logo');
fs.mkdirSync(logoDest, { recursive: true });
const logoSrc = path.join(ROOT, 'logo');
for (const f of fs.readdirSync(logoSrc)) {
  fs.copyFileSync(path.join(logoSrc, f), path.join(logoDest, f));
}

// --- PyInstaller: single exe with embedded runtime + logo ---
console.log('Building claude-rpc.exe (PyInstaller all-in-one)...');
const sep = process.platform === 'win32' ? ';' : ':';
execSync([
  'pyinstaller',
  '--onefile',
  '--windowed',
  '--icon=logo/anthropic-rpc.ico',
  '--name=claude-rpc',
  '--distpath=dist',
  '--version-file=version_info.txt',
  `--add-data=build/staging/runtime${sep}runtime`,
  `--add-data=build/staging/logo${sep}logo`,
  'main.py',
].join(' '), { cwd: ROOT, stdio: 'inherit' });

// --- Clean staging ---
fs.rmSync(STAGING, { recursive: true, force: true });

const exePath = path.join(ROOT, 'dist', 'claude-rpc.exe');
const size = (fs.statSync(exePath).size / 1024 / 1024).toFixed(1);
console.log('');
console.log(`Build complete! dist/claude-rpc.exe (${size} MB)`);
console.log('Single file — double-click to launch, tray only.');
