#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const ROOT = path.resolve(__dirname, '..');
const source = path.join(ROOT, 'src-tauri', 'target', 'release', 'claude-rpc.exe');
const fallback = path.join(ROOT, 'src-tauri', 'target', 'release', 'claude_rpc_tray.exe');
const targetDir = path.join(ROOT, 'bin');
const target = path.join(targetDir, 'claude-rpc.exe');
const selected = fs.existsSync(source) ? source : fallback;

if (!fs.existsSync(selected)) {
  console.error('[export-tauri] missing Tauri release exe');
  process.exit(1);
}

fs.rmSync(path.join(targetDir, 'runtime'), { recursive: true, force: true });
fs.rmSync(path.join(targetDir, 'claude-rpc-daemon.exe'), { force: true });
fs.mkdirSync(targetDir, { recursive: true });
fs.copyFileSync(selected, target);
console.log(`[export-tauri] ${path.relative(ROOT, target)}`);
