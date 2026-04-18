#!/usr/bin/env node
'use strict';

/**
 * tray.js - Claude Rich Presence with system tray support
 *
 * Entry points:
 *   npm start          → tray.js  (with system tray on Windows)
 *   npm run start:cli  → index.js (console only)
 *   npm run build      → all-in-one exe via PyInstaller + node.exe runtime
 *
 * On Windows: spawns a PowerShell NotifyIcon for system tray.
 * On macOS/Linux: runs in console mode (use main.py for pystray tray).
 */

const { spawn } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');

const RPC_DIR = path.join(os.homedir(), '.claude-rpc');
const STATUS_FILE = path.join(RPC_DIR, 'status.txt');
const CONFIG_PATH = path.join(RPC_DIR, 'config.json');
const IS_WINDOWS = process.platform === 'win32';

// CLI flags
const args = process.argv.slice(2);
const NO_TRAY = args.includes('--no-tray') || !IS_WINDOWS;

// --- Start the RPC engine ---

const rpc = require('./index');
const APP_NAME = rpc.TRAY_APP_NAME || 'Claude Rich Presence';
const engine = rpc.start();

if (NO_TRAY) {
  // Console-only mode - index.js handles everything
  process.exit = process.exit; // keep alive
} else {
  // --- Windows system tray via PowerShell ---
  startWindowsTray();
}

function startWindowsTray() {
  const iconPath = findIcon();
  const trayScript = path.join(RPC_DIR, 'tray-icon.ps1');

  // Write the PowerShell tray script
  fs.writeFileSync(trayScript, generateTrayScript(iconPath), 'utf8');

  const tray = spawn('powershell', [
    '-NoProfile', '-ExecutionPolicy', 'Bypass',
    '-WindowStyle', 'Hidden',
    '-File', trayScript,
  ], {
    windowsHide: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  let buffer = '';
  tray.stdout.on('data', (chunk) => {
    buffer += chunk.toString();
    const lines = buffer.split('\n');
    buffer = lines.pop();
    for (const line of lines) {
      const cmd = line.trim();
      if (cmd === 'CMD:QUIT') {
        process.exit(0);
      } else if (cmd === 'CMD:DND:ON') {
        if (engine) { engine.config.dnd = true; engine.saveConfig(engine.config); }
      } else if (cmd === 'CMD:DND:OFF') {
        if (engine) { engine.config.dnd = false; engine.saveConfig(engine.config); }
      } else if (cmd === 'CMD:BOOT:ON') {
        enableStartOnBoot();
      } else if (cmd === 'CMD:BOOT:OFF') {
        disableStartOnBoot();
      }
    }
  });

  tray.on('error', (err) => {
    console.error('Tray icon failed:', err.message);
  });

  // Cleanup tray on exit
  process.on('exit', () => {
    try { tray.kill(); } catch {}
  });
}

function findIcon() {
  const candidates = [
    path.join(path.dirname(process.execPath), 'logo', 'anthropic-rpc.ico'),
    path.join(__dirname, 'logo', 'anthropic-rpc.ico'),
    path.join(__dirname, 'dist', 'logo', 'anthropic-rpc.ico'),
  ];
  for (const p of candidates) {
    if (fs.existsSync(p)) return p;
  }
  return '';
}

function generateTrayScript(iconPath) {
  const statusFileSafe = STATUS_FILE.replace(/\\/g, '\\\\').replace(/'/g, "''");
  const configFileSafe = CONFIG_PATH.replace(/\\/g, '\\\\').replace(/'/g, "''");
  const iconPathSafe = iconPath.replace(/\\/g, '\\\\').replace(/'/g, "''");
  const appNameSafe = APP_NAME.replace(/\\/g, '\\\\').replace(/'/g, "''");

  return `
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$statusFile = '${statusFileSafe}'
$configFile = '${configFileSafe}'
$appName = '${appNameSafe}'

# Create NotifyIcon
$notifyIcon = New-Object System.Windows.Forms.NotifyIcon
$notifyIcon.Text = $appName
$notifyIcon.Visible = $true

# Try to load icon
$iconPath = '${iconPathSafe}'
if ($iconPath -and (Test-Path $iconPath)) {
    try {
        $notifyIcon.Icon = New-Object System.Drawing.Icon($iconPath)
    } catch {
        $notifyIcon.Icon = [System.Drawing.SystemIcons]::Application
    }
} else {
    $notifyIcon.Icon = [System.Drawing.SystemIcons]::Application
}

# Context menu
$contextMenu = New-Object System.Windows.Forms.ContextMenuStrip

$titleItem = $contextMenu.Items.Add($appName)
$titleItem.Enabled = $false
$titleItem.Font = New-Object System.Drawing.Font($titleItem.Font, [System.Drawing.FontStyle]::Bold)

$providerItem = $contextMenu.Items.Add("Provider: Unknown")
$providerItem.Enabled = $false

$modelItem = $contextMenu.Items.Add("Model: Auto-detect")
$modelItem.Enabled = $false

$discordItem = $contextMenu.Items.Add("Discord: RPC disabled")
$discordItem.Enabled = $false

$contextMenu.Items.Add("-") | Out-Null

$dndItem = New-Object System.Windows.Forms.ToolStripMenuItem("Do Not Disturb")
$dndItem.CheckOnClick = $true
# Read initial DND state from config
try {
    if (Test-Path $configFile) {
        $cfg = Get-Content $configFile -Raw | ConvertFrom-Json
        if ($cfg.dnd -eq $true) { $dndItem.Checked = $true }
    }
} catch {}
$contextMenu.Items.Add($dndItem) | Out-Null

$bootItem = New-Object System.Windows.Forms.ToolStripMenuItem("Start on Boot")
$bootItem.CheckOnClick = $true
# Check registry
try {
    $reg = Get-ItemProperty -Path "HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run" -Name "ClaudeRPC" -ErrorAction SilentlyContinue
    if ($reg) { $bootItem.Checked = $true }
} catch {}
$contextMenu.Items.Add($bootItem) | Out-Null

$contextMenu.Items.Add("-") | Out-Null

$quitItem = $contextMenu.Items.Add("Quit")

$notifyIcon.ContextMenuStrip = $contextMenu

# Event handlers
$quitItem.Add_Click({
    [Console]::Out.WriteLine("CMD:QUIT")
    [Console]::Out.Flush()
    $notifyIcon.Visible = $false
    [System.Windows.Forms.Application]::Exit()
})

$dndItem.Add_Click({
    if ($dndItem.Checked) {
        [Console]::Out.WriteLine("CMD:DND:ON")
    } else {
        [Console]::Out.WriteLine("CMD:DND:OFF")
    }
    [Console]::Out.Flush()
})

$bootItem.Add_Click({
    if ($bootItem.Checked) {
        [Console]::Out.WriteLine("CMD:BOOT:ON")
    } else {
        [Console]::Out.WriteLine("CMD:BOOT:OFF")
    }
    [Console]::Out.Flush()
})

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 3000
$timer.Add_Tick({
    try {
        if (Test-Path $statusFile) {
            $raw = (Get-Content $statusFile -Raw -ErrorAction SilentlyContinue).Trim()
            if ($raw) {
                try {
                    $status = $raw | ConvertFrom-Json -ErrorAction Stop
                    if ($status.providerLine) { $providerItem.Text = [string]$status.providerLine }
                    if ($status.modelLine)    { $modelItem.Text = [string]$status.modelLine }
                    if ($status.discordLine)  { $discordItem.Text = [string]$status.discordLine }
                } catch {}
            }
        }
    } catch {}
})
$timer.Start()

[System.Windows.Forms.Application]::Run()
`;
}

// --- Start on Boot (Windows Registry) ---

function getExeCommand() {
  if (process.pkg) {
    return `"${process.execPath}"`;
  }
  return `"${process.execPath}" "${path.resolve(__filename)}"`;
}

function enableStartOnBoot() {
  if (!IS_WINDOWS) return;
  try {
    const cmd = getExeCommand();
    spawn('reg', [
      'add', 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run',
      '/v', 'ClaudeRPC', '/t', 'REG_SZ', '/d', cmd, '/f',
    ], { windowsHide: true, stdio: 'ignore' });
  } catch {}
}

function disableStartOnBoot() {
  if (!IS_WINDOWS) return;
  try {
    spawn('reg', [
      'delete', 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run',
      '/v', 'ClaudeRPC', '/f',
    ], { windowsHide: true, stdio: 'ignore' });
  } catch {}
}
