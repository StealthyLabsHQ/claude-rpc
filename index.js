#!/usr/bin/env node
'use strict';

// --- CLI arguments ---

const cliArgs = process.argv.slice(2);
if (cliArgs.includes('--version') || cliArgs.includes('-v')) {
  console.log(`claude-rpc v${require('./package.json').version}`);
  process.exit(0);
}
if (cliArgs.includes('--help') || cliArgs.includes('-h')) {
  console.log(`claude-rpc v${require('./package.json').version}

Usage: claude-rpc [options]

Options:
  -v, --version    Show version number
  -h, --help       Show this help message
  --verbose        Enable verbose console output
  --dnd            Start in Do Not Disturb mode`);
  process.exit(0);
}

// --- Environment loading (silent - suppress discord-rpc handshake noise) ---

const dotenv = require('dotenv');
const originalLog = console.log;
console.log = () => {};
const { loadSecureEnv } = require('./secure-env');
const loadedSecure = loadSecureEnv();
if (!loadedSecure) {
  dotenv.config({ path: require('path').join(__dirname, '.env') }); // same dir as index.js
  dotenv.config({ path: require('path').join(require('path').dirname(process.execPath), '.env') }); // exe dir (pkg builds)
  dotenv.config(); // cwd fallback
}
console.log = originalLog;

// --- Imports ---

const { Client } = require('@xhayper/discord-rpc');
const { spawn } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');

const TRAY_APP_NAME = 'Claude Rich Presence';

// --- Logging ---

const RPC_DIR = path.join(os.homedir(), '.claude-rpc');
const LOG_FILE = path.join(RPC_DIR, 'rpc.log');

function ensureDir(dir) {
  if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
}
ensureDir(RPC_DIR);

// Rotate on startup if > 1 MB
try {
  if (fs.existsSync(LOG_FILE) && fs.statSync(LOG_FILE).size > 1_048_576) {
    const rotated = LOG_FILE + '.old';
    if (fs.existsSync(rotated)) fs.unlinkSync(rotated);
    fs.renameSync(LOG_FILE, rotated);
  }
} catch {}

const logStream = fs.createWriteStream(LOG_FILE, { flags: 'a' });

function log(level, ...msgs) {
  const ts = new Date().toISOString().slice(0, 19).replace('T', ' ');
  const line = `${ts} [${level.toUpperCase()}] ${msgs.join(' ')}`;
  logStream.write(line + '\n');
  if (VERBOSE || level === 'error') process.stderr.write(line + '\n');
}

// --- Config (~/.claude-rpc/config.json) ---

const CONFIG_PATH = path.join(RPC_DIR, 'config.json');
const DEFAULT_CONFIG = {
  logoMode: 'url',
  dnd: false,
  verbose: false,
  webhookUrl: null,
};

function loadConfig() {
  try {
    if (fs.existsSync(CONFIG_PATH)) {
      return { ...DEFAULT_CONFIG, ...JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf8')) };
    }
  } catch (e) {
    log('warn', 'Config load failed:', e.message);
  }
  return { ...DEFAULT_CONFIG };
}

function saveConfig(cfg) {
  try {
    fs.writeFileSync(CONFIG_PATH, JSON.stringify(cfg, null, 2), 'utf8');
  } catch (e) {
    log('error', 'Config save failed:', e.message);
  }
}

const config = loadConfig();
// CLI overrides
if (cliArgs.includes('--dnd')) config.dnd = true;
const VERBOSE = cliArgs.includes('--verbose') || config.verbose;

// --- Validation helpers ---

const VALID_CLIENT_ID = /^\d{17,20}$/;
const MAX_JSONL_SCAN_BYTES = 64 * 1024;
const MAX_STRING_LENGTH = 128;

function sanitizeString(str, maxLen = MAX_STRING_LENGTH) {
  if (typeof str !== 'string') return '';
  return str.replace(/[^\w\s.\-\u2022()]/g, '').slice(0, maxLen).trim();
}

function validateJsonlEntry(entry) {
  if (!entry || typeof entry !== 'object') return null;
  if (typeof entry.timestamp !== 'string' || isNaN(Date.parse(entry.timestamp))) return null;
  if (entry.type && typeof entry.type !== 'string') return null;
  if (entry.message && typeof entry.message !== 'object') return null;
  if (entry.message?.content !== undefined) {
    const c = entry.message.content;
    if (typeof c !== 'string' && !Array.isArray(c)) return null;
  }
  return entry;
}

// --- Single instance lock ---

const LOCK_FILE = path.join(RPC_DIR, 'rpc.lock');

function acquireLock() {
  try {
    fs.writeFileSync(LOCK_FILE, String(process.pid), { flag: 'wx' });
    return;
  } catch (e) {
    if (e.code !== 'EEXIST') throw e;
  }
  try {
    const pid = parseInt(fs.readFileSync(LOCK_FILE, 'utf8').trim(), 10);
    if (pid && pid !== process.pid) {
      try {
        process.kill(pid, 0);
        console.error(`Another instance is already running (PID ${pid}). Exiting.`);
        process.exit(0);
      } catch {
        // Stale lock - overwrite
      }
    }
  } catch {}
  fs.writeFileSync(LOCK_FILE, String(process.pid), 'utf8');
}

function releaseLock() {
  try {
    if (fs.existsSync(LOCK_FILE)) {
      const pid = fs.readFileSync(LOCK_FILE, 'utf8').trim();
      if (pid === String(process.pid)) fs.unlinkSync(LOCK_FILE);
    }
  } catch {}
}

// --- Init ---

function start() {
  acquireLock();
  log('info', `claude-rpc v${require('./package.json').version} starting (PID ${process.pid})`);

  const DEFAULT_CLIENT_ID = '1483898157854363799';
  const CLIENT_ID = (process.env.DISCORD_CLIENT_ID && VALID_CLIENT_ID.test(process.env.DISCORD_CLIENT_ID))
    ? process.env.DISCORD_CLIENT_ID
    : DEFAULT_CLIENT_ID;
  log('info', `Using Discord client ID: ${CLIENT_ID}${CLIENT_ID === DEFAULT_CLIENT_ID ? ' (default)' : ''}`);

  let client = new Client({ clientId: CLIENT_ID });

  const HOME = os.homedir();
  const claudeDirEnv = (process.env.CLAUDE_DIR_PATH || '~/.claude').replace(/^~/, HOME);
  const CLAUDE_DIR = path.resolve(claudeDirEnv);

  const LOGO_MODE = (process.env.DISCORD_LOGO_MODE || config.logoMode || 'url').toLowerCase();
  const LOGO_ASSET_NAME = 'claude_logo';
  const LOGO_URL = 'https://raw.githubusercontent.com/StealthyLabsHQ/claude-rpc/main/logo/discord.png';

  // --- Auto-update check ---

  const PKG_VERSION = require('./package.json').version;
  const UPDATE_REPO = 'StealthyLabsHQ/claude-rpc';

  function checkForUpdates() {
    const https = require('https');
    const options = {
      hostname: 'api.github.com',
      path: `/repos/${UPDATE_REPO}/releases/latest`,
      headers: { 'User-Agent': `claude-rpc/${PKG_VERSION}` },
      timeout: 5000,
    };
    const req = https.get(options, (res) => {
      if (res.statusCode !== 200) return;
      let body = '';
      res.on('data', (chunk) => { body += chunk; });
      res.on('end', () => {
        try {
          const release = JSON.parse(body);
          const latest = (release.tag_name || '').replace(/^v/, '');
          if (latest && latest !== PKG_VERSION && compareVersions(latest, PKG_VERSION) > 0) {
            console.log(`\nUpdate available: v${PKG_VERSION} \u2192 v${latest}`);
            console.log(`Download: https://github.com/${UPDATE_REPO}/releases/latest`);
            log('info', `Update available: v${PKG_VERSION} -> v${latest}`);
          }
        } catch {}
      });
    });
    req.on('error', () => {});
    req.on('timeout', () => { req.destroy(); });
  }

  checkForUpdates();

  // --- Persistent watcher (platform-specific) ---

  const IS_MACOS = process.platform === 'darwin';
  const IS_WINDOWS = process.platform === 'win32';
  const IS_LINUX = process.platform === 'linux';
  const WATCHER_INTERVAL_MS = 500;
  const WATCHER_SCRIPT_PATH = path.join(RPC_DIR, IS_WINDOWS ? 'claude-rpc-watcher.ps1' : 'claude-rpc-watcher.sh');
  const WATCHER_VERSION = '23';

  let watcherState = {
    client: null,
    mode: null,
    submode: null,
    model: null,
    effort: null,
    adaptive: false,
    extended: false,
    codeInstances: 0,
  };
  let watcherProcess = null;
  let watcherRestarts = 0;
  let watcherLastUpdate = 0;
  const MAX_WATCHER_RESTARTS = 10;

  function writeWatcherScript() {
    const versionMarker = IS_WINDOWS ? `# v${WATCHER_VERSION}` : `#!/bin/bash\n# v${WATCHER_VERSION}`;
    try {
      if (fs.existsSync(WATCHER_SCRIPT_PATH)) {
        const existing = fs.readFileSync(WATCHER_SCRIPT_PATH, 'utf8');
        if (existing.startsWith(versionMarker.split('\n')[0]) && existing.includes(`# v${WATCHER_VERSION}`)) return;
      }
    } catch {}

    if (IS_WINDOWS) {
      fs.writeFileSync(WATCHER_SCRIPT_PATH, `# v${WATCHER_VERSION}
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$intervalMs = ${WATCHER_INTERVAL_MS}
$appData = [Environment]::GetFolderPath("ApplicationData")
$desktopConfigPath = Join-Path $appData "Claude\\claude_desktop_config.json"

function Convert-DesktopSidebarMode($rawMode) {
    if (-not $rawMode) { return "" }
    switch ($rawMode.ToString().ToLowerInvariant()) {
        "epitaxy" { return "Cowork" }
        "cowork" { return "Cowork" }
        "chat" { return "Chat" }
        "code" { return "Code" }
        default { return "" }
    }
}

function Parse-DesktopModel($rawName) {
    if (-not $rawName) { return $null }
    $name = $rawName.ToString().Trim()
    if (-not $name) { return $null }

    $match = [regex]::Match($name, '^(?:Claude\\s+)?(Opus|Sonnet|Haiku)\\s+(\\d+\\.\\d+)(?:\\s+(1M))?(?:\\s+(Adaptive|Extended))?(?:\\s*\\u00b7\\s*(Low|Medium|High|Extra high|Max))?(?:\\s+.*)?$', [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)
    if ($match.Success) {
        $familyRaw = $match.Groups[1].Value
        $family = $familyRaw.Substring(0, 1).ToUpper() + $familyRaw.Substring(1).ToLower()
        $ctx = $match.Groups[3].Value
        $suffix = $match.Groups[4].Value
        $effort = $match.Groups[5].Value
        $modelStr = "$family $($match.Groups[2].Value)"
        if ($ctx) { $modelStr = "$modelStr $ctx" }
        return @{
            model = $modelStr
            adaptive = $suffix -ieq 'Adaptive'
            extended = $suffix -ieq 'Extended'
            effort = $effort
        }
    }

    $match = [regex]::Match($name, '^claude-(opus|sonnet|haiku)-(\\d+)-(\\d+)(?:\\[[^\\]]+\\])?$', [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)
    if ($match.Success) {
        $familyRaw = $match.Groups[1].Value
        $family = $familyRaw.Substring(0, 1).ToUpper() + $familyRaw.Substring(1).ToLower()
        return @{
            model = "$family $($match.Groups[2].Value).$($match.Groups[3].Value)"
            adaptive = $false
            extended = $false
        }
    }

    return $null
}

function Get-DesktopUiState($names, $fallbackMode) {
    $scores = @{
        Cowork = 0
        Code = 0
        Chat = 0
    }
    $state = @{
        mode = $fallbackMode
        submode = ""
        model = ""
        adaptive = $false
        extended = $false
        adaptiveSeen = $false
        extendedSeen = $false
    }
    $dispatchHit = $false

    foreach ($rawName in $names) {
        if (-not $rawName) { continue }
        $name = $rawName.ToString().Trim()
        if (-not $name) { continue }
        $lower = $name.ToLowerInvariant()

        switch ($lower) {
            'new task' { $scores.Cowork += 5; break }
            'work in a project' { $scores.Cowork += 3; break }
            'computer use' { $scores.Cowork += 4; break }
            'code permissions' { $scores.Cowork += 4; break }
            'outputs' { $scores.Cowork += 4; break }
            'keep awake' { $scores.Cowork += 4; break }
            'allow all browser actions' { $scores.Cowork += 4; break }
            'sync tasks and refresh memory' { $scores.Cowork += 3; break }
            'initialize productivity system' { $scores.Cowork += 3; break }
            'dispatch' { $scores.Cowork += 1; break }
            'scheduled' { $scores.Cowork += 1; break }
            'new session' { $scores.Code += 5; break }
            'routines' { $scores.Code += 4; break }
            'overview' { $scores.Code += 3; break }
            'models' { $scores.Code += 3; break }
            'favorite model' { $scores.Code += 3; break }
            'current streak' { $scores.Code += 3; break }
            'longest streak' { $scores.Code += 3; break }
            'peak hour' { $scores.Code += 3; break }
            'total tokens' { $scores.Code += 3; break }
            'active days' { $scores.Code += 3; break }
            'messages' { $scores.Code += 2; break }
            'sessions' { $scores.Code += 2; break }
            'new chat' { $scores.Chat += 5; break }
            'artifacts' { $scores.Chat += 4; break }
            'learn' { $scores.Chat += 4; break }
            'write' { $scores.Chat += 4; break }
            'from calendar' { $scores.Chat += 4; break }
            'from gmail' { $scores.Chat += 4; break }
        }

        if ($lower.StartsWith("what's up next") -or $lower.StartsWith('whats up next')) {
            $scores.Code += 5
        }

        if ($lower.StartsWith("let's knock something off your list") -or $lower.StartsWith('lets knock something off your list')) {
            $scores.Cowork += 6
        }

        if ($lower.StartsWith('get to work with productivity')) {
            $scores.Cowork += 3
        }

        if ($lower.StartsWith('back at it')) {
            $scores.Chat += 4
        }

        if ($lower -like 'dispatch background conversation*' -or $lower -like 'dispatch to claude and check in*' -or $lower -like 'files claude shares will appear here*') {
            $dispatchHit = $true
        }

        if ($lower -eq 'adaptive thinking') {
            $state.adaptiveSeen = $true
            if (-not $state.adaptive) { $state.adaptive = $true }
        }

        if ($lower -eq 'extended thinking') {
            $state.extendedSeen = $true
            if (-not $state.extended) { $state.extended = $true }
        }

        $parsedModel = Parse-DesktopModel $name
        if ($parsedModel) {
            if (-not $state.model) {
                $state.model = $parsedModel.model
            }
            if ($parsedModel.adaptive) {
                $state.adaptive = $true
            }
            if ($parsedModel.extended) {
                $state.extended = $true
            }
        }
    }

    $rankedModes = @(
        @{ Mode = 'Cowork'; Score = [int]$scores.Cowork },
        @{ Mode = 'Code'; Score = [int]$scores.Code },
        @{ Mode = 'Chat'; Score = [int]$scores.Chat }
    ) | Sort-Object { $_.Score } -Descending

    if ($rankedModes[0].Score -gt 0) {
        if ($rankedModes.Count -gt 1 -and $rankedModes[0].Score -eq $rankedModes[1].Score -and $fallbackMode) {
            $state.mode = $fallbackMode
        } else {
            $state.mode = $rankedModes[0].Mode
        }
    }

    if ($dispatchHit -and $state.mode -eq 'Cowork') {
        $state.submode = 'Dispatch'
    }

    return $state
}

while ($true) {
    $client = ""
    $mode = ""
    $submode = ""
    $model = ""
    $effort = ""
    $adaptive = $false
    $extended = $false
    $adaptiveSeen = $false
    $extendedSeen = $false

    $allClaude = @(Get-Process -Name 'Claude' -ErrorAction SilentlyContinue)
    $desktopFound = $false
    $codeCount = 0
    foreach ($proc in $allClaude) {
        try {
            $exePath = $proc.Path
            if (-not $exePath) {
                $codeCount++
                continue
            }
            if ($exePath -match 'WindowsApps|AnthropicClaude') {
                $desktopFound = $true
            } else {
                $codeCount++
            }
        } catch {
            $codeCount++
        }
    }

    if ($desktopFound) {
        $client = "desktop"
        $fallbackMode = ""

        try {
            if (Test-Path $desktopConfigPath) {
                $desktopConfig = Get-Content $desktopConfigPath -Raw -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop
                $fallbackMode = Convert-DesktopSidebarMode $desktopConfig.preferences.sidebarMode
            }
        } catch {}

        try {
            $w = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
                [System.Windows.Automation.TreeScope]::Children,
                (New-Object System.Windows.Automation.PropertyCondition(
                    [System.Windows.Automation.AutomationElement]::NameProperty, "Claude")))

            if ($w) {
                $all = $w.FindAll(
                    [System.Windows.Automation.TreeScope]::Descendants,
                    [System.Windows.Automation.Condition]::TrueCondition)
                $names = New-Object 'System.Collections.Generic.List[string]'
                $bestModelScore = -1
                $bestModelAdaptive = $false
                $bestModelExtended = $false
                $bestModelEffort = ""
                $toggleAdaptive = $null
                $toggleExtended = $null
                $toggleAdaptiveSeen = $false
                $toggleExtendedSeen = $false

                for ($i = 0; $i -lt $all.Count; $i++) {
                    $el = $all.Item($i)
                    $n = $el.Current.Name
                    if (-not $n) { continue }
                    $names.Add($n)

                    $candidateModel = $null
                    $candidateScore = -1
                    $controlType = $el.Current.ControlType.ProgrammaticName
                    $isOffscreen = $el.Current.IsOffscreen

                    $parsedModel = Parse-DesktopModel $n
                    if ($parsedModel) {
                        $candidateModel = $parsedModel
                        # Prefer the active-model pill: contains effort suffix (· Extra high)
                        if ($parsedModel.effort -and -not $isOffscreen -and $controlType -eq "ControlType.Button") {
                            $candidateScore = 6
                        } elseif (-not $isOffscreen -and $controlType -eq "ControlType.Button") {
                            $candidateScore = 4
                        } elseif (-not $isOffscreen) {
                            $candidateScore = 3
                        } else {
                            $candidateScore = 2
                        }
                    }

                    if ($candidateScore -gt $bestModelScore) {
                        $bestModelScore = $candidateScore
                        $model = $candidateModel.model
                        $bestModelAdaptive = $candidateModel.adaptive
                        $bestModelExtended = $candidateModel.extended
                        $bestModelEffort = $candidateModel.effort
                    }

                    $lowerName = $n.ToString().Trim().ToLowerInvariant()
                    if ($lowerName -eq 'adaptive thinking' -or $lowerName -eq 'extended thinking') {
                        $toggleOn = $null
                        try {
                            $tp = $el.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
                            if ($tp) {
                                $toggleOn = ($tp.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::On)
                            }
                        } catch {}
                        if ($toggleOn -eq $null) {
                            try {
                                $parent = [System.Windows.Automation.TreeWalker]::ControlViewWalker.GetParent($el)
                                if ($parent) {
                                    $ptp = $parent.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
                                    if ($ptp) {
                                        $toggleOn = ($ptp.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::On)
                                    }
                                }
                            } catch {}
                        }
                        if ($toggleOn -eq $null) {
                            try {
                                $kids = $el.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition)
                                for ($j = 0; $j -lt $kids.Count; $j++) {
                                    $ktp = $null
                                    try { $ktp = $kids.Item($j).GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern) } catch {}
                                    if ($ktp) {
                                        $toggleOn = ($ktp.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::On)
                                        break
                                    }
                                }
                            } catch {}
                        }
                        if ($lowerName -eq 'adaptive thinking') {
                            $toggleAdaptiveSeen = $true
                            if ($toggleOn -ne $null) { $toggleAdaptive = $toggleOn }
                        } else {
                            $toggleExtendedSeen = $true
                            if ($toggleOn -ne $null) { $toggleExtended = $toggleOn }
                        }
                    }
                }

                $uiState = Get-DesktopUiState $names $fallbackMode
                $mode = $uiState.mode
                $submode = $uiState.submode
                # Prefer actual toggle state from UIA over label presence
                if ($toggleAdaptive -ne $null) {
                    $adaptive = [bool]$toggleAdaptive
                } else {
                    $adaptive = $uiState.adaptive -or $bestModelAdaptive
                }
                if ($toggleExtended -ne $null) {
                    $extended = [bool]$toggleExtended
                } else {
                    $extended = $uiState.extended -or $bestModelExtended
                }
                $adaptiveSeen = $uiState.adaptiveSeen -or $toggleAdaptiveSeen
                $extendedSeen = $uiState.extendedSeen -or $toggleExtendedSeen
                $effort = $bestModelEffort

                if (-not $model -and $uiState.model) {
                    $model = $uiState.model
                }
            }
        } catch {}
    } elseif ($codeCount -gt 0) {
        $client = "code"
    }

    $clientSafe = $client -replace '"', ''
    $modeSafe = $mode -replace '"', ''
    $submodeSafe = $submode -replace '"', ''
    $modelSafe = $model -replace '"', ''
    $effortSafe = $effort -replace '"', ''
    $adaptiveSafe = if ($adaptive) { "true" } else { "false" }
    $extendedSafe = if ($extended) { "true" } else { "false" }
    $adaptiveSeenSafe = if ($adaptiveSeen) { "true" } else { "false" }
    $extendedSeenSafe = if ($extendedSeen) { "true" } else { "false" }
    $json = "{""client"":""" + $clientSafe + """,""mode"":""" + $modeSafe + """,""submode"":""" + $submodeSafe + """,""model"":""" + $modelSafe + """,""effort"":""" + $effortSafe + """,""adaptive"":" + $adaptiveSafe + ",""extended"":" + $extendedSafe + ",""adaptiveSeen"":" + $adaptiveSeenSafe + ",""extendedSeen"":" + $extendedSeenSafe + ",""codeInstances"":" + $codeCount + "}"
    [Console]::Out.WriteLine($json)
    [Console]::Out.Flush()
    Start-Sleep -Milliseconds $intervalMs
}
`, 'utf8');
    } else {
      // macOS + Linux watcher script
      fs.writeFileSync(WATCHER_SCRIPT_PATH, `#!/bin/bash
# v${WATCHER_VERSION}
INTERVAL_S=$(echo "scale=2; ${WATCHER_INTERVAL_MS}/1000" | bc 2>/dev/null || echo "1")

while true; do
    client=""
    mode=""
    model=""
    code_count=0

    # macOS: detect Claude Desktop (app bundle)
    if [ "$(uname)" = "Darwin" ]; then
        if pgrep -xq "Claude" 2>/dev/null; then
            client="desktop"
            mode=$(osascript -e '
                tell application "System Events"
                    if exists process "Claude" then
                        tell process "Claude"
                            try
                                set radioButtons to radio buttons of first radio group of first window
                                repeat with rb in radioButtons
                                    if value of rb is 1 then
                                        return name of rb
                                    end if
                                end repeat
                            end try
                        end tell
                    end if
                end tell
                return ""
            ' 2>/dev/null || echo "")
        fi
    fi

    # Detect Claude Code CLI (macOS + Linux)
    if [ -z "$client" ]; then
        code_count=$(pgrep -fc "[c]laude" 2>/dev/null || echo "0")
        # Exclude this watcher script from the count
        if [ "$code_count" -gt 0 ] 2>/dev/null; then
            client="code"
        fi
    fi

    # Sanitize values
    client=\${client//\\"/}
    mode=\${mode//\\"/}
    model=\${model//\\"/}

    echo "{\\"client\\":\\"$client\\",\\"mode\\":\\"$mode\\",\\"model\\":\\"$model\\",\\"codeInstances\\":$code_count}"

    sleep $INTERVAL_S
done
`, 'utf8');
      fs.chmodSync(WATCHER_SCRIPT_PATH, 0o755);
    }
  }

  function startWatcher() {
    writeWatcherScript();

    if (IS_WINDOWS) {
      watcherProcess = spawn('powershell', [
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', WATCHER_SCRIPT_PATH
      ], { windowsHide: true, stdio: ['ignore', 'pipe', 'ignore'] });
    } else {
      watcherProcess = spawn('/bin/bash', [WATCHER_SCRIPT_PATH], {
        stdio: ['ignore', 'pipe', 'ignore'],
      });
    }

    let buffer = '';
    watcherProcess.stdout.on('data', (chunk) => {
      buffer += chunk.toString();
      const lines = buffer.split('\n');
      buffer = lines.pop();
      for (const line of lines) {
        const trimmed = line.trim();
        if (!trimmed || !trimmed.startsWith('{')) continue;
        try {
          const data = JSON.parse(trimmed);
          watcherLastUpdate = Date.now();
          const prevModel = watcherState.model;
          const prevMode = watcherState.mode;
          watcherState.client = data.client || null;
          watcherState.mode = data.mode || null;
          watcherState.submode = data.submode || null;
          watcherState.codeInstances = data.codeInstances || 0;
          watcherState.effort = data.effort || null;

          const nextModel = data.model || null;
          const modelChanged = nextModel !== prevModel;
          const modeChanged = watcherState.mode !== prevMode;
          watcherState.model = nextModel;

          // Preserve adaptive/extended across ticks when the thinking toggles
          // aren't visible in the UI (e.g. model picker closed). Reset when
          // mode/model changes or the picker exposes a contradicting state.
          if (data.adaptiveSeen || modelChanged || modeChanged) {
            watcherState.adaptive = !!data.adaptive;
          }
          if (data.extendedSeen || modelChanged || modeChanged) {
            watcherState.extended = !!data.extended;
          }
        } catch {}
      }
    });

    watcherProcess.on('exit', (code) => {
      watcherState = {
        client: null,
        mode: null,
        submode: null,
        model: null,
        adaptive: false,
        extended: false,
        codeInstances: 0,
      };
      watcherLastUpdate = 0;
      watcherRestarts++;
      if (watcherRestarts > MAX_WATCHER_RESTARTS) {
        log('error', `Watcher max restarts (${MAX_WATCHER_RESTARTS}) exceeded`);
        console.error(`\nWatcher max restarts (${MAX_WATCHER_RESTARTS}) exceeded`);
        return;
      }
      const delay = Math.min(2000 * Math.pow(2, watcherRestarts - 1), 60000);
      log('warn', `Watcher exited (code ${code}), restarting in ${delay}ms`);
      console.error(`\nWatcher exited (code ${code}), restarting in ${delay}ms...`);
      setTimeout(startWatcher, delay);
    });
  }

  function stopWatcher() {
    if (watcherProcess) {
      watcherProcess.kill();
      watcherProcess = null;
    }
  }

  // --- Client detection ---

  function detectClient() {
    if (Date.now() - watcherLastUpdate > 5000) return null;
    return watcherState.client;
  }

  function detectDesktopInfo() {
    return {
      mode: watcherState.mode,
      submode: watcherState.submode,
      model: watcherState.model,
      effort: watcherState.effort,
      adaptive: watcherState.adaptive,
      extended: watcherState.extended,
    };
  }

  // --- Detection: API provider (with TTL cache) ---

  let cachedProvider = null;
  let providerCachedAt = 0;
  const PROVIDER_CACHE_TTL = 5 * 60 * 1000; // 5 minutes

  function readSettingsEnv() {
    try {
      const settingsPath = path.join(CLAUDE_DIR, 'settings.json');
      if (fs.existsSync(settingsPath)) {
        const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
        return settings && typeof settings.env === 'object' ? settings.env : {};
      }
    } catch {}
    return {};
  }

  function truthy(v) {
    return v === '1' || v === 'true' || v === true;
  }

  function detectProvider() {
    if (cachedProvider && (Date.now() - providerCachedAt) < PROVIDER_CACHE_TTL) {
      return cachedProvider;
    }

    let result = 'Unknown';
    const settingsEnv = readSettingsEnv();
    const lookup = (key) => process.env[key] ?? settingsEnv[key];

    if (truthy(lookup('CLAUDE_CODE_USE_BEDROCK'))) {
      result = 'Amazon Bedrock';
    } else if (truthy(lookup('CLAUDE_CODE_USE_VERTEX'))) {
      result = 'Google GCP Vertex';
    } else if (truthy(lookup('CLAUDE_CODE_USE_FOUNDRY'))) {
      result = 'Microsoft Foundry';
    } else if (lookup('ANTHROPIC_API_KEY') || lookup('CLAUDE_API_KEY')) {
      result = 'Anthropic API';
    } else {
      try {
        const configPath = path.join(CLAUDE_DIR, 'config.json');
        if (fs.existsSync(configPath)) {
          const raw = fs.readFileSync(configPath, 'utf8');
          if (raw.includes('"sk-ant-')) result = 'Anthropic API';
        }
      } catch (e) {
        log('warn', 'Provider detection (config):', e.message);
      }

      if (result === 'Unknown') {
        try {
          const credsPath = path.join(CLAUDE_DIR, '.credentials.json');
          if (fs.existsSync(credsPath)) {
            const raw = fs.readFileSync(credsPath, 'utf8');
            if (raw.includes('"claudeAiOauth"')) result = 'Claude Account';
          }
        } catch (e) {
          log('warn', 'Provider detection (creds):', e.message);
        }
      }
    }

    cachedProvider = result;
    providerCachedAt = Date.now();
    return result;
  }

  // --- Detection: model ---

  const CODE_EFFORT_MAP = {
    low: 'Low',
    medium: 'Medium',
    high: 'High',
    xhigh: 'Extra high',
    extrahigh: 'Extra high',
    'extra high': 'Extra high',
    max: 'Max',
  };

  function detectCodeEffort() {
    try {
      const settingsPath = path.join(CLAUDE_DIR, 'settings.json');
      if (fs.existsSync(settingsPath)) {
        const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
        const raw = (settings.effortLevel || '').toString().toLowerCase().trim();
        if (raw) return CODE_EFFORT_MAP[raw] || null;
      }
    } catch {}
    return null;
  }

  function appendCodeEffort(model) {
    if (!model) return model;
    const effort = detectCodeEffort();
    if (!effort) return model;
    if (new RegExp(`\\b${effort}\\b`, 'i').test(model)) return model;
    return `${model} \u00b7 ${effort}`;
  }

  function detectModel(clientType, sessionFile) {
    if (clientType === 'desktop') {
      const info = detectDesktopInfo();
      if (info.model) {
        return `Claude ${info.model}`;
      }
    }

    let base = null;

    try {
      const settingsPath = path.join(CLAUDE_DIR, 'settings.json');
      if (fs.existsSync(settingsPath)) {
        const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
        if (typeof settings.model === 'string') base = formatModelName(settings.model);
      }
    } catch (e) {
      log('warn', 'Model detection (settings):', e.message);
    }

    if (!base && sessionFile) {
      try {
        const tail = readFileTail(sessionFile, MAX_JSONL_SCAN_BYTES);
        const lines = tail.split('\n');
        for (let i = lines.length - 1; i >= 0; i--) {
          try {
            const entry = JSON.parse(lines[i]);
            if (entry.type === 'assistant' && entry.message?.model) {
              base = formatModelName(entry.message.model);
              break;
            }
          } catch {}
        }
      } catch {}
    }

    if (!base && process.env.CLAUDE_MODEL) base = formatModelName(process.env.CLAUDE_MODEL);
    if (!base && process.env.ANTHROPIC_MODEL) base = formatModelName(process.env.ANTHROPIC_MODEL);

    return base;
  }

  // --- Detection: project name ---

  function detectProjectName(sessionFilePath) {
    if (!sessionFilePath) return null;

    const dirName = path.basename(path.dirname(sessionFilePath));
    const wtIdx = dirName.indexOf('--claude-worktrees-');
    const encoded = wtIdx >= 0 ? dirName.slice(0, wtIdx) : dirName;

    let root, parts;

    const winMatch = encoded.match(/^([a-zA-Z])--(.+)$/);
    const unixMatch = !winMatch && encoded.match(/^-?(.+)$/);

    if (winMatch) {
      root = winMatch[1] + ':\\';
      parts = winMatch[2].split('-').filter(Boolean);
    } else if (unixMatch) {
      root = '/';
      parts = unixMatch[1].split('-').filter(Boolean);
    } else {
      return null;
    }

    let cur = root;
    let i = 0;
    while (i < parts.length) {
      let found = false;
      for (let j = i; j < parts.length; j++) {
        const name = parts.slice(i, j + 1).join('-');
        const full = path.join(cur, name);
        try {
          if (fs.statSync(full).isDirectory()) {
            cur = full;
            i = j + 1;
            found = true;
            break;
          }
        } catch {}
      }
      if (!found) {
        return parts.slice(i).join('-') || path.basename(cur);
      }
    }
    return path.basename(cur);
  }

  // --- Detection: thinking state ---

  function detectThinkingState(sessionFile) {
    if (!sessionFile) return false;
    try {
      const stat = fs.statSync(sessionFile);
      if (Date.now() - stat.mtimeMs > 10_000) return false;

      const tail = readFileTail(sessionFile, 4 * 1024);
      const lines = tail.split('\n').filter(Boolean);
      if (lines.length === 0) return false;

      for (let i = lines.length - 1; i >= Math.max(0, lines.length - 3); i--) {
        try {
          const entry = JSON.parse(lines[i]);
          if (entry.type === 'assistant' && Array.isArray(entry.message?.content)) {
            for (const block of entry.message.content) {
              if (block.type === 'thinking') return true;
            }
          }
        } catch {}
      }
    } catch {}
    return false;
  }

  // --- Build activity ---

  function getLogoImage() {
    return LOGO_MODE === 'asset' ? LOGO_ASSET_NAME : LOGO_URL;
  }

  function buildActivity(clientType, sessionStats, projectName, isThinking, model) {
    const desktopInfo = clientType === 'desktop' ? detectDesktopInfo() : null;
    const desktopModeLabel = desktopInfo?.mode
      ? formatDesktopModeLabel(desktopInfo.mode, desktopInfo.submode)
      : null;
    const codePresenceLabels = getCodePresenceLabels();

    if (clientType === 'desktop') {
      model = formatDesktopModelLabel(model, {
        adaptive: desktopInfo?.adaptive,
        extended: desktopInfo?.extended,
        effort: desktopInfo?.mode === 'Code' ? desktopInfo?.effort : null,
      });
    }

    const logoImage = getLogoImage();
    const provider = detectProvider();

    const configs = {
      code: {
        details: codePresenceLabels.details,
        state: `${model || 'Claude'} | ${provider}`,
        largeImageKey: logoImage,
        largeImageText: model || 'Claude Code',
        smallImageKey: 'terminal_icon',
        smallImageText: codePresenceLabels.smallImageText,
        buttons: [{ label: 'Claude', url: 'https://claude.ai' }, { label: 'GitHub', url: 'https://github.com/StealthyLabsHQ/claude-rpc' }],
      },
      desktop: {
        details: `Claude Desktop (${desktopModeLabel || 'Chat'})`,
        state: `${model || 'Claude'} | ${provider}`,
        largeImageKey: logoImage,
        largeImageText: 'Powered by Anthropic',
        smallImageKey: 'terminal_icon',
        smallImageText: `Claude Desktop${desktopModeLabel ? ' - ' + desktopModeLabel : ''}`,
        buttons: [{ label: 'Claude Desktop', url: 'https://claude.ai/download' }, { label: 'GitHub', url: 'https://github.com/StealthyLabsHQ/claude-rpc' }],
      },
    };

    return configs[clientType];
  }

  // --- Session file scanning (recursive, depth-limited) ---

  function findLatestJsonlFile() {
    const projectsDir = path.join(CLAUDE_DIR, 'projects');
    if (!fs.existsSync(projectsDir)) return null;

    const MAX_DEPTH = 3;
    const EXCLUDE_DIRS = new Set(['node_modules', '.git', '.venv', '__pycache__', 'dist', 'build']);

    let latestFile = null;
    let latestMtime = 0;

    function scanDir(dir, depth) {
      if (depth > MAX_DEPTH) return;
      try {
        const entries = fs.readdirSync(dir, { withFileTypes: true });
        for (const entry of entries) {
          if (EXCLUDE_DIRS.has(entry.name)) continue;
          const full = path.join(dir, entry.name);
          if (entry.isDirectory()) {
            scanDir(full, depth + 1);
          } else if (entry.name.endsWith('.jsonl')) {
            try {
              const mtime = fs.statSync(full).mtimeMs;
              if (mtime > latestMtime) {
                latestMtime = mtime;
                latestFile = full;
              }
            } catch {}
          }
        }
      } catch {}
    }

    scanDir(projectsDir, 0);
    return latestFile;
  }

  // --- Detection: session start time ---

  function getSessionStartTime() {
    try {
      const latestFile = findLatestJsonlFile();

      if (latestFile) {
        const fd = fs.openSync(latestFile, 'r');
        const buffer = Buffer.alloc(4096);
        const bytesRead = fs.readSync(fd, buffer, 0, 4096, 0);
        fs.closeSync(fd);
        const lines = buffer.toString('utf8', 0, bytesRead).split('\n');

        for (const line of lines) {
          if (!line.trim()) continue;
          try {
            const entry = JSON.parse(line);
            const ts = entry.timestamp || entry.snapshot?.timestamp;
            if (typeof ts === 'string' && !isNaN(Date.parse(ts))) {
              return { timestamp: Math.floor(new Date(ts).getTime() / 1000), file: latestFile };
            }
          } catch {}
        }
      }
    } catch (e) {
      log('warn', 'Session time detection:', e.message);
    }
    if (cachedSessionStart && cachedSessionFile) {
      return { timestamp: cachedSessionStart, file: cachedSessionFile };
    }
    return { timestamp: Math.floor(Date.now() / 1000), file: null };
  }

  // --- Detection: session stats ---

  function getSessionStats(sessionFile) {
    if (!sessionFile) return null;
    try {
      const tail = readFileTail(sessionFile, 256 * 1024);
      const lines = tail.split('\n').filter(Boolean);
      let edits = 0, cmds = 0, userMsgs = 0, firstUserMsg = '';
      for (const line of lines) {
        try {
          const entry = validateJsonlEntry(JSON.parse(line));
          if (!entry) continue;
          if (entry.type === 'user') {
            userMsgs++;
            if (!firstUserMsg && entry.message?.content) {
              const text = Array.isArray(entry.message.content)
                ? entry.message.content.find(c => c.type === 'text')?.text
                : entry.message.content;
              if (text) firstUserMsg = text.slice(0, 128);
            }
          }
          if (entry.type === 'assistant' && Array.isArray(entry.message?.content)) {
            for (const block of entry.message.content) {
              if (block.type === 'tool_use') {
                if (block.name === 'Edit' || block.name === 'Write' || block.name === 'NotebookEdit') edits++;
                if (block.name === 'Bash') cmds++;
              }
            }
          }
        } catch {}
      }
      return { edits, cmds, depth: userMsgs, description: firstUserMsg };
    } catch {
      return null;
    }
  }

  // --- File system watcher ---

  let sessionDirty = true;
  let fsWatcher = null;

  function startFsWatcher() {
    const projectsDir = path.join(CLAUDE_DIR, 'projects');
    if (!fs.existsSync(projectsDir)) return;
    try {
      fsWatcher = fs.watch(projectsDir, { recursive: true }, (eventType, filename) => {
        if (filename && filename.endsWith('.jsonl')) sessionDirty = true;
      });
      fsWatcher.on('error', () => { sessionDirty = true; });
    } catch {
      sessionDirty = true;
    }
  }

  function stopFsWatcher() {
    if (fsWatcher) { fsWatcher.close(); fsWatcher = null; }
  }

  // --- Webhook notifications ---

  let lastWebhookEvent = null;

  function sendWebhook(event, details) {
    if (!config.webhookUrl) return;
    if (event === lastWebhookEvent) return; // Deduplicate
    lastWebhookEvent = event;

    try {
      const url = new URL(config.webhookUrl);
      const payload = JSON.stringify({
        content: null,
        embeds: [{
          title: `${TRAY_APP_NAME} - ${event}`,
          description: sanitizeString(details || '', 256),
          color: event === 'Session Started' ? 0x6C63FF : 0x999999,
          timestamp: new Date().toISOString(),
        }],
      });
      const https = require('https');
      const http = url.protocol === 'http:' ? require('http') : https;
      const req = http.request(url, { method: 'POST', headers: { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(payload) } });
      req.on('error', () => {});
      req.write(payload);
      req.end();
      log('info', `Webhook sent: ${event}`);
    } catch (e) {
      log('warn', 'Webhook failed:', e.message);
    }
  }

  // --- Main loop ---

  const UPDATE_INTERVAL = 1_000;
  let triggerUpdate = () => {};

  let currentClient = null;
  let cachedSessionStart = null;
  let cachedSessionFile = null;
  let cachedProjectName = null;
  let cachedSessionStats = null;
  let cachedModel = null;
  let lastActivityHash = null;

  // Start watchers
  startWatcher();
  startFsWatcher();

  // --- Discord reconnection logic ---

  let rpcConnected = false;
  let reconnectTimer = null;
  let reconnectAttempts = 0;
  const MAX_RECONNECT_DELAY = 30_000;

  function scheduleReconnect(delay) {
    if (reconnectTimer) return;
    const d = delay ?? Math.min(5000 * Math.pow(1.5, reconnectAttempts), MAX_RECONNECT_DELAY);
    reconnectAttempts++;
    log('info', `Discord disconnected, reconnecting in ${Math.round(d / 1000)}s`);
    console.log(`\nDiscord disconnected - reconnecting in ${Math.round(d / 1000)}s...`);
    updateTrayStatus(currentClient || detectClient());
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      client = new Client({ clientId: CLIENT_ID });
      setupClient();
      client.login().catch(() => scheduleReconnect());
    }, d);
  }

  // --- Status file for tray.js ---

  const STATUS_FILE = path.join(RPC_DIR, 'status.txt');
  let lastStatusPayload = '';
  let lastConsoleStatusLine = '';

  function writeStatus(status) {
    try { fs.writeFileSync(STATUS_FILE, JSON.stringify(status), 'utf8'); } catch {}
  }

  function updateTrayStatus(forceClientType) {
    const cl = forceClientType === undefined ? detectClient() : forceClientType;
    const di = detectDesktopInfo();
    let model = cl === 'desktop' ? detectModel(cl, cachedSessionFile) : cachedModel;
    if (cl === 'desktop') {
      model = formatDesktopModelLabel(model, {
        adaptive: di?.adaptive,
        extended: di?.extended,
        effort: di?.mode === 'Code' ? di?.effort : null,
      });
    }

    const status = buildTrayStatus({
      clientType: cl,
      clientMode: di?.mode || null,
      clientSubmode: di?.submode || null,
      model,
      provider: detectProvider(),
      projectName: cachedProjectName,
      codeInstances: watcherState.codeInstances,
      discordConnected: rpcConnected,
      reconnecting: !!reconnectTimer,
      dnd: !!config.dnd,
    });
    const consoleLine = buildConsoleStatusLine({
      clientType: cl,
      clientMode: di?.mode || null,
      clientSubmode: di?.submode || null,
      model,
      projectName: cachedProjectName,
      codeInstances: watcherState.codeInstances,
      dnd: !!config.dnd,
    });

    const payload = JSON.stringify(status);
    if (payload !== lastStatusPayload) {
      lastStatusPayload = payload;
      writeStatus(status);
    }
    if (consoleLine !== lastConsoleStatusLine) {
      lastConsoleStatusLine = consoleLine;
      process.stdout.write(`\r\x1b[2K${consoleLine}`);
    }
  }

  function setupClient() {
    client.on('ready', () => {
      rpcConnected = true;
      reconnectAttempts = 0;
      log('info', 'Discord Rich Presence connected');
      console.log('Rich Presence ready');
      console.log(`Provider: ${detectProvider()}`);
      updateTrayStatus();
      currentClient = detectClient();

      function update() {
        // Do Not Disturb: clear presence and skip
        if (config.dnd) {
          if (lastActivityHash !== 'dnd') {
            client.user.clearActivity();
            lastActivityHash = 'dnd';
            log('info', 'DND mode active');
          }
          updateTrayStatus(detectClient());
          return;
        }

        const detected = detectClient();
        updateTrayStatus(detected);

        if (detected) {
          if (detected !== currentClient) {
            const prev = currentClient;
            currentClient = detected;
            cachedSessionStart = null;
            cachedSessionFile = null;
            cachedProjectName = null;
            cachedSessionStats = null;
            cachedModel = null;
            lastActivityHash = null;
            sessionDirty = true;
            log('info', `Client changed: ${prev} -> ${detected}`);
            sendWebhook('Session Started', `Client: ${detected}`);
          }

          if (sessionDirty) {
            sessionDirty = false;
            if (detected === 'desktop') {
              if (!cachedSessionStart) cachedSessionStart = Math.floor(Date.now() / 1000);
              cachedModel = detectModel(detected, null);
            } else {
              const sessionInfo = getSessionStartTime();
              if (sessionInfo.file !== cachedSessionFile) {
                cachedSessionFile = sessionInfo.file;
                cachedSessionStart = sessionInfo.timestamp;
                cachedProjectName = detectProjectName(cachedSessionFile);
                lastActivityHash = null;
              }
              cachedSessionStats = getSessionStats(cachedSessionFile);
              cachedModel = detectModel(detected, cachedSessionFile);
            }
          }

          if (detected === 'desktop' && watcherState.model) {
            const clean = watcherState.model.replace(/\s*(Adaptive|Extended)\s*/ig, ' ').trim();
            cachedModel = formatDesktopModelLabel(clean, {
              adaptive: watcherState.adaptive,
              extended: watcherState.extended,
              effort: watcherState.mode === 'Code' ? watcherState.effort : null,
            });
          }

          const isThinking = detected === 'code' ? detectThinkingState(cachedSessionFile) : false;
          const displayModel = detected === 'code' ? appendCodeEffort(cachedModel) : cachedModel;
          const a = buildActivity(detected, cachedSessionStats, cachedProjectName, isThinking, displayModel);
          const activityPayload = {
            type: 0,
            name: 'Claude AI',
            details: a.details,
            state: a.state,
            startTimestamp: cachedSessionStart,
            assets: {
              largeImage: a.largeImageKey,
              largeText: a.largeImageText,
              smallImage: a.smallImageKey,
              smallText: a.smallImageText,
            },
            buttons: a.buttons,
          };

          const nextActivityHash = buildPresenceChangeKey(activityPayload);
          if (nextActivityHash !== lastActivityHash) {
            lastActivityHash = nextActivityHash;
            client.user.setActivity(activityPayload);
          }
        } else {
          if (currentClient !== null) {
            updateTrayStatus(detected);
            sendWebhook('Session Ended', `${cachedModel || 'Claude'} session ended`);
            currentClient = null;
            cachedSessionStart = null;
            cachedSessionFile = null;
            cachedProjectName = null;
            cachedSessionStats = null;
            lastActivityHash = null;
          }

          // No Claude session → clear Discord presence (no Idle placeholder)
          if (lastActivityHash !== 'cleared') {
            client.user.clearActivity();
            lastActivityHash = 'cleared';
          }
        }
      }

      triggerUpdate = update;
      update();
      setInterval(update, UPDATE_INTERVAL);
    });

    client.on('disconnected', () => {
      rpcConnected = false;
      lastActivityHash = null;
      updateTrayStatus(currentClient || detectClient());
      scheduleReconnect();
    });

    client.on('error', () => {
      if (!rpcConnected) {
        updateTrayStatus(currentClient || detectClient());
        scheduleReconnect();
      }
    });
  }

  // Wait for first watcher data before connecting
  setTimeout(() => {
    setupClient();
    client.login().catch(() => scheduleReconnect(5000));
  }, WATCHER_INTERVAL_MS + 200);
  updateTrayStatus();

  // Cleanup
  function cleanup() {
    log('info', 'Shutting down');
    if (reconnectTimer) { clearTimeout(reconnectTimer); reconnectTimer = null; }
    stopWatcher();
    stopFsWatcher();
    releaseLock();
    logStream.end();
    try { fs.unlinkSync(STATUS_FILE); } catch {}
  }

  process.on('SIGINT', () => { cleanup(); process.exit(0); });
  process.on('SIGTERM', () => { cleanup(); process.exit(0); });
  process.on('exit', () => { cleanup(); });

  // Expose config for tray.js
  return { config, saveConfig };
}

// --- Shared utilities (available without starting) ---

function readFileTail(filePath, bytes) {
  const stat = fs.statSync(filePath);
  const size = stat.size;
  if (size <= bytes) return fs.readFileSync(filePath, 'utf8');
  const fd = fs.openSync(filePath, 'r');
  const buffer = Buffer.alloc(bytes);
  fs.readSync(fd, buffer, 0, bytes, size - bytes);
  fs.closeSync(fd);
  const content = buffer.toString('utf8');
  const firstNewline = content.indexOf('\n');
  return firstNewline >= 0 ? content.slice(firstNewline + 1) : content;
}

const MODELS_1M_DEFAULT = new Set(['opus-4-6', 'opus-4-5']);
const DESKTOP_MODE_MAP = { Chat: 'Chat', Cowork: 'Cowork', Code: 'Code' };
const DESKTOP_SIDEBAR_MODE_MAP = {
  chat: 'Chat',
  cowork: 'Cowork',
  code: 'Code',
  epitaxy: 'Cowork',
};
const DESKTOP_UI_SCORE_CUES = {
  Cowork: [
    [/^new task$/, 5],
    [/^work in a project$/, 3],
    [/^computer use$/, 4],
    [/^code permissions$/, 4],
    [/^outputs$/, 4],
    [/^keep awake$/, 4],
    [/^allow all browser actions$/, 4],
    [/^sync tasks and refresh memory$/, 3],
    [/^initialize productivity system$/, 3],
    [/^dispatch$/, 1],
    [/^scheduled$/, 1],
    [/^let's knock something off your list/, 6],
    [/^lets knock something off your list/, 6],
    [/^get to work with productivity/, 3],
  ],
  Code: [
    [/^new session$/, 5],
    [/^routines$/, 4],
    [/^what's up next/, 5],
    [/^whats up next/, 5],
    [/^overview$/, 3],
    [/^models$/, 3],
    [/^favorite model$/, 3],
    [/^current streak$/, 3],
    [/^longest streak$/, 3],
    [/^peak hour$/, 3],
    [/^total tokens$/, 3],
    [/^active days$/, 3],
    [/^messages$/, 2],
    [/^sessions$/, 2],
  ],
  Chat: [
    [/^new chat$/, 5],
    [/^artifacts$/, 4],
    [/^learn$/, 4],
    [/^write$/, 4],
    [/^from calendar$/, 4],
    [/^from gmail$/, 4],
    [/^back at it/, 4],
  ],
};

function formatModelName(modelId) {
  if (!modelId || typeof modelId !== 'string') return null;
  const id = modelId.toLowerCase().slice(0, 100);

  const versionMatch = id.match(/(\d+)[_-](\d+)/);
  const version = versionMatch ? `${versionMatch[1]}.${versionMatch[2]}` : '';

  const ctxMatch = id.match(/\[(\d+m)\]/i);
  let ctx = ctxMatch ? ` (${ctxMatch[1].toUpperCase()})` : '';

  if (!ctx && version) {
    const family = id.includes('opus') ? 'opus' : id.includes('sonnet') ? 'sonnet' : id.includes('haiku') ? 'haiku' : null;
    if (family && MODELS_1M_DEFAULT.has(`${family}-${version.replace('.', '-')}`)) {
      ctx = ' (1M)';
    }
  }

  const latestVersions = { opus: '4.6', sonnet: '4.6', haiku: '4.5' };

  if (id.includes('opusplan')) return 'Opus Plan / Sonnet 4.6';
  if (id.includes('opus')) return `Claude Opus ${version || latestVersions.opus}${ctx}`.trim();
  if (id.includes('sonnet')) return `Claude Sonnet ${version || latestVersions.sonnet}${ctx}`.trim();
  if (id.includes('haiku')) return `Claude Haiku ${version || latestVersions.haiku}${ctx}`.trim();
  return sanitizeString(modelId);
}

function getCodePresenceLabels() {
  return {
    details: 'Claude Code',
    smallImageText: 'Claude Code CLI',
  };
}

function mapDesktopSidebarMode(rawMode) {
  const normalized = sanitizeString(rawMode || '', 32).toLowerCase();
  return DESKTOP_SIDEBAR_MODE_MAP[normalized] || null;
}

function normalizeDesktopUiLabel(name) {
  return sanitizeString(name || '', 128).toLowerCase();
}

function parseDesktopModelCandidate(rawValue) {
  const cleanValue = sanitizeString(rawValue || '', 64);
  if (!cleanValue) return null;

  let match = cleanValue.match(/^(?:Claude\s+)?(Opus|Sonnet|Haiku)\s+(\d+\.\d+)(?:\s+(1M))?(?:\s+(Adaptive|Extended))?(?:\s+.*)?$/i);
  if (match) {
    const family = match[1][0].toUpperCase() + match[1].slice(1).toLowerCase();
    const ctx = match[3] || '';
    const suffix = match[4] || '';
    return {
      model: ctx ? `${family} ${match[2]} ${ctx}` : `${family} ${match[2]}`,
      adaptive: /adaptive/i.test(suffix),
      extended: /extended/i.test(suffix),
    };
  }

  match = cleanValue.match(/^claude-(opus|sonnet|haiku)-(\d+)-(\d+)(?:\[[^\]]+\])?$/i);
  if (match) {
    const family = match[1][0].toUpperCase() + match[1].slice(1).toLowerCase();
    return {
      model: `${family} ${match[2]}.${match[3]}`,
      adaptive: false,
      extended: false,
    };
  }

  return null;
}

function inferDesktopUiState(names, fallbackMode = null) {
  const state = {
    mode: mapDesktopSidebarMode(fallbackMode) || null,
    submode: null,
    model: null,
    adaptive: false,
    extended: false,
  };

  if (!Array.isArray(names) || names.length === 0) {
    return state;
  }

  const scores = { Cowork: 0, Code: 0, Chat: 0 };
  let dispatchHit = false;

  for (const rawName of names) {
    const normalized = normalizeDesktopUiLabel(rawName);
    if (!normalized) continue;

    for (const [mode, cues] of Object.entries(DESKTOP_UI_SCORE_CUES)) {
      for (const [pattern, weight] of cues) {
        if (pattern.test(normalized)) {
          scores[mode] += weight;
        }
      }
    }

    if (
      normalized.startsWith('dispatch background conversation') ||
      normalized.startsWith('dispatch to claude and check in') ||
      normalized.startsWith('files claude shares will appear here')
    ) {
      dispatchHit = true;
    }

    if (normalized === 'adaptive thinking') {
      state.adaptive = true;
    }

    if (normalized === 'extended thinking') {
      state.extended = true;
    }

    const modelCandidate = parseDesktopModelCandidate(rawName);
    if (modelCandidate) {
      if (!state.model) {
        state.model = modelCandidate.model;
      }
      state.adaptive = state.adaptive || modelCandidate.adaptive;
      state.extended = state.extended || modelCandidate.extended;
    }
  }

  const rankedModes = Object.entries(scores).sort((a, b) => b[1] - a[1]);
  const [bestMode, bestScore] = rankedModes[0] || [null, 0];
  const [, secondScore] = rankedModes[1] || [null, 0];

  if (bestScore > 0) {
    state.mode = bestScore === secondScore
      ? (mapDesktopSidebarMode(fallbackMode) || state.mode)
      : bestMode;
  }

  if (dispatchHit && state.mode === 'Cowork') {
    state.submode = 'Dispatch';
  }

  return state;
}

function formatDesktopModeLabel(mode, submode = null) {
  const baseMode = DESKTOP_MODE_MAP[mode] || sanitizeString(mode || '') || 'Chat';
  const cleanSubmode = sanitizeString(submode || '', 32);
  return cleanSubmode ? `${baseMode} - ${cleanSubmode}` : baseMode;
}

function formatDesktopModelLabel(model, {
  adaptive = false,
  extended = false,
  effort = null,
} = {}) {
  let cleanModel = sanitizeString(model || '') || null;
  if (!cleanModel) return null;

  const hasAdaptive = /\bAdaptive\b/i.test(cleanModel);
  const hasExtended = /\bExtended\b/i.test(cleanModel);

  if (!hasAdaptive && !hasExtended) {
    if (adaptive) cleanModel = `${cleanModel} Adaptive`;
    else if (extended) cleanModel = `${cleanModel} Extended`;
  }

  const cleanEffort = sanitizeString(effort || '', 32);
  if (cleanEffort && !new RegExp(`\\b${cleanEffort}\\b`, 'i').test(cleanModel)) {
    cleanModel = `${cleanModel} \u00b7 ${cleanEffort}`;
  }

  return cleanModel;
}

function buildPresenceChangeKey(activityPayload = {}) {
  const assets = activityPayload.assets || {};
  const buttons = Array.isArray(activityPayload.buttons) ? activityPayload.buttons : [];

  return JSON.stringify({
    type: activityPayload.type || null,
    name: typeof activityPayload.name === 'string' ? activityPayload.name.trim().slice(0, 64) : null,
    details: typeof activityPayload.details === 'string'
      ? activityPayload.details.replace(/\s+\(thinking\.\.\.\)$/i, '').trim().slice(0, 128)
      : null,
    state: typeof activityPayload.state === 'string' ? activityPayload.state.trim().slice(0, 128) : null,
    startTimestamp: Number.isFinite(activityPayload.startTimestamp) ? activityPayload.startTimestamp : null,
    assets: {
      largeImage: typeof assets.largeImage === 'string' ? assets.largeImage.slice(0, 256) : null,
      largeText: typeof assets.largeText === 'string' ? assets.largeText.trim().slice(0, 128) : null,
      smallImage: typeof assets.smallImage === 'string' ? assets.smallImage.slice(0, 256) : null,
      smallText: typeof assets.smallText === 'string' ? assets.smallText.trim().slice(0, 128) : null,
    },
    buttons: buttons.map((button) => ({
      label: typeof button?.label === 'string' ? button.label.trim().slice(0, 64) : null,
      url: typeof button?.url === 'string' ? button.url.slice(0, 256) : null,
    })),
  });
}

function buildConsoleStatusLine({
  clientType = null,
  clientMode = null,
  model = null,
  projectName = null,
  codeInstances = 0,
  dnd = false,
} = {}) {
  let runtime = 'Idle';
  if (clientType === 'desktop') {
    runtime = `Claude Desktop${clientMode ? ` • ${DESKTOP_MODE_MAP[clientMode] || clientMode}` : ''}`;
  } else if (clientType === 'code') {
    const instanceSuffix = codeInstances > 1 ? ` [${codeInstances}]` : '';
    const projectSuffix = projectName ? ` \u2022 ${projectName}` : '';
    runtime = `Claude Code${instanceSuffix}${projectSuffix}`;
  }

  const dndTag = dnd ? ' [DND]' : '';
  return `${runtime} • ${model || 'auto-detect'}${dndTag}`;
}

function buildTrayStatus() {
  return {
    version: 2,
    summary: TRAY_APP_NAME,
  };
}

// Keep tray UI intentionally minimal: title + provider/model/Discord only.
function buildConsoleStatusLine({
  clientType = null,
  clientMode = null,
  clientSubmode = null,
  model = null,
  projectName = null,
  codeInstances = 0,
  dnd = false,
} = {}) {
  let runtime = 'Idle';
  if (clientType === 'desktop') {
    runtime = `Claude Desktop${clientMode ? ` | ${formatDesktopModeLabel(clientMode, clientSubmode)}` : ''}`;
  } else if (clientType === 'code') {
    const instanceSuffix = codeInstances > 1 ? ` [${codeInstances}]` : '';
    const projectSuffix = projectName ? ` | ${projectName}` : '';
    runtime = `Claude Code${instanceSuffix}${projectSuffix}`;
  }

  const dndTag = dnd ? ' [DND]' : '';
  return `${runtime} | ${model || 'auto-detect'}${dndTag}`;
}

function buildTrayStatus({
  clientType = null,
  clientMode = null,
  clientSubmode = null,
  model = null,
  provider = null,
  discordConnected = false,
} = {}) {
  let claudeLine;
  if (!clientType) {
    claudeLine = 'Claude: Off';
  } else if (clientType === 'code') {
    claudeLine = 'Claude: CLI (Code)';
  } else {
    const modeLabel = clientMode
      ? (clientSubmode ? `${clientMode} - ${clientSubmode}` : clientMode)
      : null;
    claudeLine = modeLabel ? `Claude: Desktop (${modeLabel})` : 'Claude: Desktop';
  }

  return {
    version: 4,
    summary: TRAY_APP_NAME,
    claudeLine,
    modelLine: model || 'Auto-detect',
    providerLine: `Provider: ${provider || 'Unknown'}`,
    discordLine: `Discord: ${discordConnected ? 'Connected' : 'RPC disabled'}`,
  };
}

function compareVersions(a, b) {
  const pa = a.split('.').map(Number);
  const pb = b.split('.').map(Number);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const na = pa[i] || 0;
    const nb = pb[i] || 0;
    if (na > nb) return 1;
    if (na < nb) return -1;
  }
  return 0;
}

// --- Entry point ---

if (require.main === module) {
  start();
}

module.exports = {
  start,
  formatModelName,
  inferDesktopUiState,
  formatDesktopModeLabel,
  formatDesktopModelLabel,
  buildPresenceChangeKey,
  compareVersions,
  sanitizeString,
  loadConfig,
  saveConfig,
  DEFAULT_CONFIG,
  CONFIG_PATH,
  buildTrayStatus,
  getCodePresenceLabels,
  TRAY_APP_NAME,
};
