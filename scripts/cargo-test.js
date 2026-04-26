#!/usr/bin/env node
'use strict';

const { spawnSync } = require('node:child_process');
const path = require('node:path');

const cargo = process.env.CARGO
  || (process.platform === 'win32'
    ? path.join(process.env.USERPROFILE || '', '.cargo', 'bin', 'cargo.exe')
    : 'cargo');

const result = spawnSync(cargo, ['test', '--manifest-path', 'src-tauri/Cargo.toml'], {
  stdio: 'inherit',
});

process.exit(result.status ?? 1);
