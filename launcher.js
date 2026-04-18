#!/usr/bin/env node
'use strict';

/**
 * launcher.js — Pure Node.js alternative to main.py (experimental).
 * Single instance + system tray (PowerShell) + RPC engine, no Python needed.
 * NOTE: Not used in the default build. main.py + PyInstaller is the production path.
 */

const net   = require('net');
const fs    = require('fs');
const path  = require('path');
const os    = require('os');
const { spawn } = require('child_process');

const IS_WINDOWS = process.platform === 'win32';
const RPC_DIR    = path.join(os.homedir(), '.claude-rpc');
const STATUS_FILE = path.join(RPC_DIR, 'status.txt');
const CONFIG_PATH = path.join(RPC_DIR, 'config.json');
const APP_NAME = 'Claude Rich Presence';

fs.mkdirSync(RPC_DIR, { recursive: true });

// ─── Tray icon (read from logo/tray-icon.b64, cache as PNG) ─────────────────

function ensureIcon() {
    const dest = path.join(RPC_DIR, 'tray-icon.png');
    if (!fs.existsSync(dest)) {
        const b64Path = path.join(__dirname, 'logo', 'tray-icon.b64');
        const b64 = fs.readFileSync(b64Path, 'utf8').trim();
        fs.writeFileSync(dest, Buffer.from(b64, 'base64'));
    }
    return dest;
}

// ─── Single instance (Windows named pipe) ─────────────────────────────────────

const PIPE_NAME = '\\\\.\\pipe\\ClaudeRPC_SingleInstance';

function acquireSingleInstance() {
    return new Promise((resolve) => {
        const srv = net.createServer();
        srv.on('error', () => process.exit(0)); // already running
        srv.listen(PIPE_NAME, () => resolve(srv));
    });
}

// ─── Start on boot ────────────────────────────────────────────────────────────

function isStartupEnabled() {
    if (!IS_WINDOWS) return false;
    try {
        const { execSync } = require('child_process');
        execSync(
            'reg query "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run" /v ClaudeRPC',
            { stdio: 'ignore' }
        );
        return true;
    } catch { return false; }
}

function enableStartup() {
    if (!IS_WINDOWS) return;
    spawn('reg', [
        'add', 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run',
        '/v', 'ClaudeRPC', '/t', 'REG_SZ', '/d', `"${process.execPath}"`, '/f',
    ], { windowsHide: true, stdio: 'ignore' });
}

function disableStartup() {
    if (!IS_WINDOWS) return;
    spawn('reg', [
        'delete', 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run',
        '/v', 'ClaudeRPC', '/f',
    ], { windowsHide: true, stdio: 'ignore' });
}

// ─── PowerShell tray ──────────────────────────────────────────────────────────

function generateTrayScript(iconPath) {
    const icon   = iconPath.replace(/\\/g, '\\\\').replace(/'/g, "''");
    const status = STATUS_FILE.replace(/\\/g, '\\\\').replace(/'/g, "''");
    const config = CONFIG_PATH.replace(/\\/g, '\\\\').replace(/'/g, "''");
    const appName = APP_NAME.replace(/\\/g, '\\\\').replace(/'/g, "''");

    return `
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$appName = '${appName}'
$statusFile = '${status}'
$configFile  = '${config}'

$notifyIcon = New-Object System.Windows.Forms.NotifyIcon
$notifyIcon.Text    = $appName
$notifyIcon.Visible = $true

$iconPath = '${icon}'
if ($iconPath -and (Test-Path $iconPath)) {
    try { $notifyIcon.Icon = [System.Drawing.Icon]::FromHandle(([System.Drawing.Bitmap]::FromFile($iconPath)).GetHicon()) }
    catch { $notifyIcon.Icon = [System.Drawing.SystemIcons]::Application }
} else { $notifyIcon.Icon = [System.Drawing.SystemIcons]::Application }

$contextMenu = New-Object System.Windows.Forms.ContextMenuStrip
$titleItem   = $contextMenu.Items.Add($appName)
$titleItem.Enabled = $false
$titleItem.Font    = New-Object System.Drawing.Font($titleItem.Font, [System.Drawing.FontStyle]::Bold)

$providerItem = $contextMenu.Items.Add("Provider: Unknown")
$providerItem.Enabled = $false

$modelItem = $contextMenu.Items.Add("Model: Auto-detect")
$modelItem.Enabled = $false

$discordItem = $contextMenu.Items.Add("Discord: RPC disabled")
$discordItem.Enabled = $false
$contextMenu.Items.Add("-") | Out-Null

$dndItem = New-Object System.Windows.Forms.ToolStripMenuItem("Do Not Disturb")
$dndItem.CheckOnClick = $true
try {
    if (Test-Path $configFile) {
        $cfg = Get-Content $configFile -Raw | ConvertFrom-Json
        if ($cfg.dnd -eq $true) { $dndItem.Checked = $true }
    }
} catch {}
$contextMenu.Items.Add($dndItem) | Out-Null

$bootItem = New-Object System.Windows.Forms.ToolStripMenuItem("Start on Boot")
$bootItem.CheckOnClick = $true
try {
    $reg = Get-ItemProperty "HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run" -Name "ClaudeRPC" -EA SilentlyContinue
    if ($reg) { $bootItem.Checked = $true }
} catch {}
$contextMenu.Items.Add($bootItem) | Out-Null

$contextMenu.Items.Add("-") | Out-Null
$quitItem = $contextMenu.Items.Add("Quit")
$notifyIcon.ContextMenuStrip = $contextMenu

$quitItem.Add_Click({
    [Console]::Out.WriteLine("CMD:QUIT"); [Console]::Out.Flush()
    $notifyIcon.Visible = $false
    [System.Windows.Forms.Application]::Exit()
})
$dndItem.Add_Click({
    if ($dndItem.Checked) { [Console]::Out.WriteLine("CMD:DND:ON") }
    else                  { [Console]::Out.WriteLine("CMD:DND:OFF") }
    [Console]::Out.Flush()
})
$bootItem.Add_Click({
    if ($bootItem.Checked) { [Console]::Out.WriteLine("CMD:BOOT:ON") }
    else                   { [Console]::Out.WriteLine("CMD:BOOT:OFF") }
    [Console]::Out.Flush()
})

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 3000
$timer.Add_Tick({
    try {
        if (Test-Path $statusFile) {
            $raw = (Get-Content $statusFile -Raw -EA SilentlyContinue).Trim()
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

function startTray(engine) {
    if (!IS_WINDOWS) return null;

    const iconPath      = ensureIcon();
    const trayScriptPath = path.join(RPC_DIR, 'tray-icon.ps1');
    fs.writeFileSync(trayScriptPath, generateTrayScript(iconPath), 'utf8');

    const tray = spawn('powershell', [
        '-NoProfile', '-ExecutionPolicy', 'Bypass',
        '-WindowStyle', 'Hidden', '-File', trayScriptPath,
    ], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });

    let buf = '';
    tray.stdout.on('data', (chunk) => {
        buf += chunk.toString();
        const lines = buf.split('\n');
        buf = lines.pop();
        for (const line of lines) {
            const cmd = line.trim();
            if      (cmd === 'CMD:QUIT')     { tray.kill(); process.exit(0); }
            else if (cmd === 'CMD:DND:ON')   { if (engine) { engine.config.dnd = true;  engine.saveConfig(engine.config); } }
            else if (cmd === 'CMD:DND:OFF')  { if (engine) { engine.config.dnd = false; engine.saveConfig(engine.config); } }
            else if (cmd === 'CMD:BOOT:ON')  { enableStartup(); }
            else if (cmd === 'CMD:BOOT:OFF') { disableStartup(); }
        }
    });

    tray.on('error', (e) => console.error('Tray error:', e.message));
    process.on('exit', () => { try { tray.kill(); } catch {} });

    return tray;
}

// ─── Entry point ──────────────────────────────────────────────────────────────

acquireSingleInstance().then(() => {
    const rpc    = require('./index');
    const engine = rpc.start();
    const tray   = startTray(engine);

    const cleanup = () => { try { if (tray) tray.kill(); } catch {} process.exit(0); };
    process.on('SIGINT',  cleanup);
    process.on('SIGTERM', cleanup);
});
