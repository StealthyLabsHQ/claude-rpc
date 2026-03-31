const dotenv = require('dotenv');
const originalLog = console.log;
console.log = () => {};
// Try encrypted .env first (DPAPI), then fall back to plaintext .env
const { loadSecureEnv } = require('./secure-env');
const loadedSecure = loadSecureEnv();
if (!loadedSecure) {
  // Load .env from exe directory (for pkg builds) or project root
  dotenv.config({ path: require('path').join(require('path').dirname(process.execPath), '.env') });
  dotenv.config(); // fallback to cwd
}
console.log = originalLog;
const { Client } = require('@xhayper/discord-rpc');
const { spawn } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');

// --- Validation helpers ---

const VALID_CLIENT_ID = /^\d{17,20}$/;
const MAX_JSONL_SCAN_BYTES = 64 * 1024;
const MAX_STRING_LENGTH = 128;

function sanitizeString(str, maxLen = MAX_STRING_LENGTH) {
  if (typeof str !== 'string') return '';
  return str.replace(/[^\w\s.\-•()]/g, '').slice(0, maxLen).trim();
}

// JSONL entry validation — returns null if entry is malformed
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

const LOCK_FILE = path.join(os.homedir(), '.claude-rpc', 'rpc.lock');

function acquireLock() {
  const lockDir = path.dirname(LOCK_FILE);
  if (!fs.existsSync(lockDir)) fs.mkdirSync(lockDir, { recursive: true, mode: 0o700 });

  // Try atomic create (fails if file exists)
  try {
    fs.writeFileSync(LOCK_FILE, String(process.pid), { flag: 'wx' });
    return; // Lock acquired
  } catch (e) {
    if (e.code !== 'EEXIST') throw e;
  }

  // File exists — check if the owning process is still alive
  try {
    const pid = parseInt(fs.readFileSync(LOCK_FILE, 'utf8').trim(), 10);
    if (pid && pid !== process.pid) {
      try {
        process.kill(pid, 0);
        console.error(`Another instance is already running (PID ${pid}). Exiting.`);
        process.exit(0);
      } catch {
        // Process is dead, stale lock — overwrite
      }
    }
  } catch {}

  fs.writeFileSync(LOCK_FILE, String(process.pid), 'utf8');
}

function releaseLock() {
  try {
    if (fs.existsSync(LOCK_FILE)) {
      const pid = fs.readFileSync(LOCK_FILE, 'utf8').trim();
      if (pid === String(process.pid)) {
        fs.unlinkSync(LOCK_FILE);
      }
    }
  } catch {}
}

acquireLock();

// --- Init ---

const CLIENT_ID = process.env.DISCORD_CLIENT_ID;
if (!CLIENT_ID || !VALID_CLIENT_ID.test(CLIENT_ID)) {
  console.error('DISCORD_CLIENT_ID missing or invalid in .env (expected 17-20 digit snowflake)');
  process.exit(1);
}

let client = new Client({ clientId: CLIENT_ID });

const HOME = os.homedir();
const claudeDirEnv = (process.env.CLAUDE_DIR_PATH || '~/.claude').replace(/^~/, HOME);
const CLAUDE_DIR = path.resolve(claudeDirEnv);

// Logo: 'asset' uses Discord Developer Portal asset name, 'url' uses GitHub-hosted image
const LOGO_MODE = (process.env.DISCORD_LOGO_MODE || 'url').toLowerCase();
const LOGO_ASSET_NAME = 'claude_logo';
const LOGO_URL = 'https://raw.githubusercontent.com/StealthyLabsHQ/anthropic-rich-presence/main/logo/discord.png';

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
          console.log(`\nUpdate available: v${PKG_VERSION} → v${latest}`);
          console.log(`Download: https://github.com/${UPDATE_REPO}/releases/latest`);
        }
      } catch {}
    });
  });
  req.on('error', () => {}); // Silent fail
  req.on('timeout', () => { req.destroy(); });
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

checkForUpdates();

// --- Persistent watcher (platform-specific) ---
// Single long-running process that detects client, mode, and model every ~1.5s

const IS_MACOS = process.platform === 'darwin';
const IS_WINDOWS = process.platform === 'win32';
const WATCHER_INTERVAL_MS = 1000;
const WATCHER_SCRIPT_DIR = path.join(os.homedir(), '.claude-rpc');
if (!fs.existsSync(WATCHER_SCRIPT_DIR)) fs.mkdirSync(WATCHER_SCRIPT_DIR, { recursive: true, mode: 0o700 });
const WATCHER_SCRIPT_PATH = path.join(WATCHER_SCRIPT_DIR, IS_WINDOWS ? 'claude-rpc-watcher.ps1' : 'claude-rpc-watcher.sh');
const WATCHER_VERSION = '10';

let watcherState = { client: null, mode: null, model: null, extended: false, codeInstances: 0 };
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

while ($true) {
    $client = ""
    $mode = ""
    $model = ""

    # Detect processes by exe path (not window title) to avoid
    # Desktop background processes being misidentified as Code CLI
    $allClaude = @(Get-Process -Name 'Claude' -ErrorAction SilentlyContinue)
    $desktopFound = $false
    $codeCount = 0
    foreach ($proc in $allClaude) {
        try {
            $exePath = $proc.Path
            if (-not $exePath) {
                # No path info — assume Code (CLI processes sometimes lack path)
                $codeCount++
                continue
            }
            if ($exePath -match 'WindowsApps|AnthropicClaude') {
                $desktopFound = $true
            } else {
                # Any Claude process that is not Desktop = Claude Code
                $codeCount++
            }
        } catch {
            $codeCount++
        }
    }

    if ($desktopFound) {
        $client = "desktop"

        # UI Automation for mode + model
        try {
            $w = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
                [System.Windows.Automation.TreeScope]::Children,
                (New-Object System.Windows.Automation.PropertyCondition(
                    [System.Windows.Automation.AutomationElement]::NameProperty, "Claude")))

            if ($w) {
                # Mode detection (RadioButtons)
                $radioCondition = New-Object System.Windows.Automation.PropertyCondition(
                    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
                    [System.Windows.Automation.ControlType]::RadioButton)
                $radios = $w.FindAll([System.Windows.Automation.TreeScope]::Descendants, $radioCondition)
                foreach ($r in $radios) {
                    $n = $r.Current.Name
                    if ($n -eq "Chat" -or $n -eq "Cowork" -or $n -eq "Code") {
                        try {
                            $sp = $r.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern)
                            if ($sp.Current.IsSelected) { $mode = $n }
                        } catch {}
                    }
                }

                # Model detection (Button starting with Opus/Sonnet/Haiku)
                $btnCondition = New-Object System.Windows.Automation.PropertyCondition(
                    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
                    [System.Windows.Automation.ControlType]::Button)
                $buttons = $w.FindAll([System.Windows.Automation.TreeScope]::Descendants, $btnCondition)
                foreach ($b in $buttons) {
                    $n = $b.Current.Name
                    if ($n -match "^(Opus|Sonnet|Haiku)") { $model = $n }
                }
            }
        } catch {}
    } elseif ($codeCount -gt 0) {
        $client = "code"
    }

    # Escape double quotes in values to prevent JSON injection
    $clientSafe = $client -replace '"', ''
    $modeSafe = $mode -replace '"', ''
    $modelSafe = $model -replace '"', ''
    $json = "{""client"":""" + $clientSafe + """,""mode"":""" + $modeSafe + """,""model"":""" + $modelSafe + """,""codeInstances"":" + $codeCount + "}"
    [Console]::Out.WriteLine($json)
    [Console]::Out.Flush()
    Start-Sleep -Milliseconds $intervalMs
}
`, 'utf8');
  } else {
    // macOS / Linux watcher script
    fs.writeFileSync(WATCHER_SCRIPT_PATH, `#!/bin/bash
# v${WATCHER_VERSION}
INTERVAL_S=$(echo "scale=2; ${WATCHER_INTERVAL_MS}/1000" | bc)

while true; do
    client=""
    mode=""
    model=""
    code_count=0

    # Detect Claude Desktop (macOS app bundle)
    if pgrep -xq "Claude"; then
        desktop_pid=$(pgrep -x "Claude" | head -1)
        if [ -n "$desktop_pid" ]; then
            client="desktop"
            # Try AppleScript for mode/model detection (requires accessibility)
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

    # Detect Claude Code (cli process named "claude")
    if [ -z "$client" ]; then
        code_count=$(pgrep -fc "claude" 2>/dev/null || echo "0")
        if [ "$code_count" -gt 0 ] 2>/dev/null; then
            client="code"
        fi
    fi

    # Sanitize values (remove quotes)
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
    buffer = lines.pop(); // Keep incomplete line in buffer
    for (const line of lines) {
      const trimmed = line.trim();
      if (!trimmed || !trimmed.startsWith('{')) continue;
      try {
        const data = JSON.parse(trimmed);
        watcherLastUpdate = Date.now();
        watcherState.client = data.client || null;
        watcherState.mode = data.mode || null;
        watcherState.codeInstances = data.codeInstances || 0;

        if (data.model) {
          // Store the raw UI label (e.g. "Opus 3", "Sonnet 4.6 Extended")
          watcherState.model = data.model;
          watcherState.extended = /extended/i.test(data.model);
        } else {
          watcherState.model = null;
          watcherState.extended = false;
        }
      } catch {}
    }
  });

  watcherProcess.on('exit', (code) => {
    watcherState = { client: null, mode: null, model: null, extended: false, codeInstances: 0 };
    watcherLastUpdate = 0;
    watcherRestarts++;
    if (watcherRestarts > MAX_WATCHER_RESTARTS) {
      console.error(`\nWatcher max restarts (${MAX_WATCHER_RESTARTS}) exceeded`);
      return;
    }
    const delay = Math.min(2000 * Math.pow(2, watcherRestarts - 1), 60000);
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

// Client detection from watcher state
function detectClient() {
  // If watcher hasn't reported in 5s, assume no client detected
  if (Date.now() - watcherLastUpdate > 5000) return null;
  return watcherState.client;
}

// Desktop info from watcher state
function detectDesktopInfo() {
  return {
    mode: watcherState.mode,
    model: watcherState.model,
    extended: watcherState.extended,
  };
}

const DESKTOP_MODE_MAP = {
  Chat: 'Chat',
  Cowork: 'Cowork',
  Code: 'Code',
};

// --- Detection: API provider (cached — only reads files once) ---

let cachedProvider = null;

function detectProvider() {
  if (cachedProvider) return cachedProvider;

  if (process.env.CLAUDE_CODE_USE_BEDROCK === '1' || process.env.CLAUDE_CODE_USE_BEDROCK === 'true') {
    return (cachedProvider = 'Amazon Bedrock');
  }
  if (process.env.CLAUDE_CODE_USE_VERTEX === '1' || process.env.CLAUDE_CODE_USE_VERTEX === 'true') {
    return (cachedProvider = 'Google Cloud Vertex');
  }
  if (process.env.CLAUDE_CODE_USE_FOUNDRY === '1' || process.env.CLAUDE_CODE_USE_FOUNDRY === 'true') {
    return (cachedProvider = 'Microsoft Foundry');
  }

  try {
    const configPath = path.join(CLAUDE_DIR, 'config.json');
    if (fs.existsSync(configPath)) {
      const raw = fs.readFileSync(configPath, 'utf8');
      if (raw.includes('"sk-ant-')) return (cachedProvider = 'Anthropic API');
    }
  } catch (e) {
    console.error('Provider detection (config):', e.message);
  }

  try {
    const credsPath = path.join(CLAUDE_DIR, '.credentials.json');
    if (fs.existsSync(credsPath)) {
      const raw = fs.readFileSync(credsPath, 'utf8');
      if (raw.includes('"claudeAiOauth"')) return (cachedProvider = 'Claude.ai');
    }
  } catch (e) {
    console.error('Provider detection (creds):', e.message);
  }

  return (cachedProvider = 'Anthropic');
}

// --- Detection: model ---

function detectModel(clientType, sessionFile) {
  // Priority 0: Claude Desktop — use raw UI label from watcher
  if (clientType === 'desktop') {
    const info = detectDesktopInfo();
    if (info.model) {
      const clean = info.model.replace(/\s*Extended\s*/i, '').trim();
      return `Claude ${clean}`;
    }
  }

  // Priority 1: Claude Code settings.json
  try {
    const settingsPath = path.join(CLAUDE_DIR, 'settings.json');
    if (fs.existsSync(settingsPath)) {
      const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
      if (typeof settings.model === 'string') {
        return formatModelName(settings.model);
      }
    }
  } catch (e) {
    console.error('Model detection (settings):', e.message);
  }

  // Priority 2: Read model from cached session file (no full rescan)
  if (sessionFile) {
    try {
      const tail = readFileTail(sessionFile, MAX_JSONL_SCAN_BYTES);
      const lines = tail.split('\n');
      for (let i = lines.length - 1; i >= 0; i--) {
        try {
          const entry = JSON.parse(lines[i]);
          if (entry.type === 'assistant' && entry.message?.model) {
            return formatModelName(entry.message.model);
          }
        } catch {}
      }
    } catch {}
  }

  // Priority 3: env vars
  if (process.env.CLAUDE_MODEL) return formatModelName(process.env.CLAUDE_MODEL);
  if (process.env.ANTHROPIC_MODEL) return formatModelName(process.env.ANTHROPIC_MODEL);
  return null;
}

function readFileTail(filePath, bytes) {
  const stat = fs.statSync(filePath);
  const size = stat.size;
  if (size <= bytes) {
    return fs.readFileSync(filePath, 'utf8');
  }
  const fd = fs.openSync(filePath, 'r');
  const buffer = Buffer.alloc(bytes);
  fs.readSync(fd, buffer, 0, bytes, size - bytes);
  fs.closeSync(fd);
  const content = buffer.toString('utf8');
  const firstNewline = content.indexOf('\n');
  return firstNewline >= 0 ? content.slice(firstNewline + 1) : content;
}

// Models with 1M context by default (no [1m] suffix needed)
const MODELS_1M_DEFAULT = new Set(['opus-4-6', 'opus-4-5']);

function formatModelName(modelId) {
  if (!modelId || typeof modelId !== 'string') return null;
  const id = modelId.toLowerCase().slice(0, 100);

  const versionMatch = id.match(/(\d+)[_-](\d+)/);
  const version = versionMatch ? `${versionMatch[1]}.${versionMatch[2]}` : '';

  // Explicit [1m] in model ID
  const ctxMatch = id.match(/\[(\d+m)\]/i);
  let ctx = ctxMatch ? ` (${ctxMatch[1].toUpperCase()})` : '';

  // Auto-detect 1M for models that have it by default
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

// --- Detection: project name (Claude Code) ---

function detectProjectName(sessionFilePath) {
  if (!sessionFilePath) return null;

  const dirName = path.basename(path.dirname(sessionFilePath));

  // Strip worktree suffix (e.g., "--claude-worktrees-focused-thompson")
  const wtIdx = dirName.indexOf('--claude-worktrees-');
  const encoded = wtIdx >= 0 ? dirName.slice(0, wtIdx) : dirName;

  let root, parts;

  // Windows: "D--Users-stealthy-..." → root = "D:\", rest segments
  const winMatch = encoded.match(/^([a-zA-Z])--(.+)$/);
  // Unix: "-Users-stealthy-..." or "Users-stealthy-..."
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

  // Greedy filesystem resolution: try single segment first, then join with dashes
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
      // Remaining parts form the project name (directory may have been deleted)
      return parts.slice(i).join('-') || path.basename(cur);
    }
  }

  return path.basename(cur);
}

// --- Detection: thinking state (Claude Code) ---

function detectThinkingState(sessionFile) {
  if (!sessionFile) return false;
  try {
    const stat = fs.statSync(sessionFile);
    // File must have been modified in the last 10 seconds (actively streaming)
    if (Date.now() - stat.mtimeMs > 10_000) return false;

    // Read last 4KB to find the last entry
    const tail = readFileTail(sessionFile, 4 * 1024);
    const lines = tail.split('\n').filter(Boolean);
    if (lines.length === 0) return false;

    // Check last few lines for thinking blocks
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
  const desktopModeLabel = desktopInfo?.mode ? DESKTOP_MODE_MAP[desktopInfo.mode] : null;

  if (clientType === 'desktop' && desktopInfo?.extended && model && !model.includes('Extended')) {
    model = model + ' Extended';
  }

  const logoImage = getLogoImage();

  const provider = detectProvider();
  let codeDetails = 'Claude Code';

  const configs = {
    code: {
      details: codeDetails,
      state: `${model || 'Claude'} | ${provider}`,
      largeImageKey: logoImage,
      largeImageText: model || 'Claude Code',
      smallImageKey: 'terminal_icon',
      smallImageText: 'Claude Code CLI',
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
    away: {
      details: 'Away',
      state: `${model || 'Claude'} | ${provider} · inactive`,
      largeImageKey: logoImage,
      largeImageText: 'Away',
      smallImageKey: 'terminal_icon',
      smallImageText: 'No recent activity',
      buttons: [{ label: 'Claude', url: 'https://claude.ai' }, { label: 'GitHub', url: 'https://github.com/StealthyLabsHQ/claude-rpc' }],
    },
    idle: {
      details: 'Idle',
      state: 'No active Claude session',
      largeImageKey: logoImage,
      largeImageText: 'Powered by Anthropic',
      smallImageKey: null,
      smallImageText: null,
      buttons: [{ label: 'Claude', url: 'https://claude.ai' }, { label: 'GitHub', url: 'https://github.com/StealthyLabsHQ/claude-rpc' }],
    },
  };

  return configs[clientType];
}

// --- Session file scanning (shared by session time + model detection) ---

function findLatestJsonlFile() {
  const projectsDir = path.join(CLAUDE_DIR, 'projects');
  if (!fs.existsSync(projectsDir)) return null;

  let latestFile = null;
  let latestMtime = 0;

  const projectDirs = fs.readdirSync(projectsDir).map(d => path.join(projectsDir, d));
  for (const dir of projectDirs) {
    try {
      if (!fs.statSync(dir).isDirectory()) continue;
      const files = fs.readdirSync(dir).filter(f => f.endsWith('.jsonl'));
      for (const file of files) {
        const filePath = path.join(dir, file);
        const mtime = fs.statSync(filePath).mtimeMs;
        if (mtime > latestMtime) {
          latestMtime = mtime;
          latestFile = filePath;
        }
      }
    } catch {}
  }

  return latestFile;
}

// --- Detection: session start time ---

function getSessionStartTime() {
  try {
    const latestFile = findLatestJsonlFile();

    if (latestFile) {
      // Read first few KB to find the earliest timestamp
      const fd = fs.openSync(latestFile, 'r');
      const buffer = Buffer.alloc(4096);
      const bytesRead = fs.readSync(fd, buffer, 0, 4096, 0);
      fs.closeSync(fd);
      const lines = buffer.toString('utf8', 0, bytesRead).split('\n');

      for (const line of lines) {
        if (!line.trim()) continue;
        try {
          const entry = JSON.parse(line);
          // Handle file-history-snapshot (timestamp in snapshot.timestamp)
          const ts = entry.timestamp || entry.snapshot?.timestamp;
          if (typeof ts === 'string' && !isNaN(Date.parse(ts))) {
            return { timestamp: Math.floor(new Date(ts).getTime() / 1000), file: latestFile };
          }
        } catch {}
      }
    }
  } catch (e) {
    console.error('Session time detection:', e.message);
  }
  // Keep cached value if available — never reset to Date.now()
  if (cachedSessionStart && cachedSessionFile) {
    return { timestamp: cachedSessionStart, file: cachedSessionFile };
  }
  return { timestamp: Math.floor(Date.now() / 1000), file: null };
}

// --- Detection: session stats (Claude Code only) ---

function getSessionStats(sessionFile) {
  if (!sessionFile) return null;
  try {
    // Read only last 256KB to avoid memory issues on large sessions
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
      } catch (_) { /* skip malformed lines */ }
    }
    return { edits, cmds, depth: userMsgs, description: firstUserMsg };
  } catch (e) {
    return null;
  }
}

// --- Idle timeout ---

const IDLE_TIMEOUT_MS = (parseInt(process.env.IDLE_TIMEOUT_MINUTES, 10) || 15) * 60 * 1000;

function isSessionIdle(sessionFile) {
  if (!sessionFile) return true;
  try {
    const mtime = fs.statSync(sessionFile).mtimeMs;
    return (Date.now() - mtime) > IDLE_TIMEOUT_MS;
  } catch {
    return true;
  }
}

// --- File system watcher (replaces polling for session files) ---

let sessionDirty = true; // Start dirty to force initial scan
let fsWatcher = null;

function startFsWatcher() {
  const projectsDir = path.join(CLAUDE_DIR, 'projects');
  if (!fs.existsSync(projectsDir)) return;

  try {
    fsWatcher = fs.watch(projectsDir, { recursive: true }, (eventType, filename) => {
      if (filename && filename.endsWith('.jsonl')) {
        sessionDirty = true;
      }
    });
    fsWatcher.on('error', () => {
      // Watcher failed, fall back to always-dirty (polling behavior)
      sessionDirty = true;
    });
  } catch {
    // fs.watch not supported or failed — fall back to polling
    sessionDirty = true;
  }
}

function stopFsWatcher() {
  if (fsWatcher) {
    fsWatcher.close();
    fsWatcher = null;
  }
}

// --- Main loop ---

const UPDATE_INTERVAL = 2_000; // 2 seconds

let currentClient = null;
let cachedSessionStart = null;
let cachedSessionFile = null;
let cachedProjectName = null;
let cachedSessionStats = null;
let cachedModel = null;
let lastActivityHash = null;
let lastActivityTime = 0;
const MIN_ACTIVITY_INTERVAL = 60_000; // Minimum 60s between Discord updates to prevent timer reset

function hashActivity(obj) {
  return JSON.stringify(obj);
}

// Start the persistent watcher and fs watcher before connecting
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
  console.log(`\nDiscord disconnected — reconnecting in ${Math.round(d / 1000)}s...`);
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    // Recreate client — the old IPC socket is dead after Discord restarts
    client = new Client({ clientId: CLIENT_ID });
    setupClient();
    client.login().catch(() => scheduleReconnect());
  }, d);
}

function setupClient() {
  client.on('ready', () => {
    rpcConnected = true;
    reconnectAttempts = 0;
    const initialClient = detectClient();
    console.log('Rich Presence ready');
    console.log(`Provider: ${detectProvider()}`);

    const clientDisplayName = { code: 'Claude Code', desktop: 'Claude Desktop' };
    // Status line: updated in-place every cycle
    let lastStatusLine = '';
    function updateStatusLine() {
      const di = detectDesktopInfo();
      const cl = detectClient();
      // For desktop, model comes from watcher (UI) so detect live; for code, use cached
      let model = cl === 'desktop' ? detectModel(cl, cachedSessionFile) : cachedModel;
      if (cl === 'desktop' && di?.extended && model && !model.includes('Extended')) {
        model += ' Extended';
      }
      const modePart = cl === 'desktop' && di?.mode ? ` • ${DESKTOP_MODE_MAP[di.mode] || di.mode}` : '';
      const projectPart = cl === 'code' && cachedProjectName ? ` • ${cachedProjectName}` : '';
      const instancePart = cl === 'code' && watcherState.codeInstances > 1 ? ` [${watcherState.codeInstances}]` : '';
      const line = `${clientDisplayName[cl] || 'Idle'}${modePart}${projectPart}${instancePart} • ${model || 'auto-detect'}`;
      if (line !== lastStatusLine) {
        lastStatusLine = line;
        process.stdout.write(`\r\x1b[2K${line}`);
      }
    }
    updateStatusLine();
    currentClient = initialClient;

    function update() {
      // Do Not Disturb: clear presence and skip update
      if (global.dndMode) {
        if (lastActivityHash !== 'dnd') {
          client.user.clearActivity();
          lastActivityHash = 'dnd';
          process.stdout.write('\r\x1b[2KDo Not Disturb');
        }
        return;
      }

      const detected = detectClient();
      updateStatusLine();

      if (detected) {
        if (detected !== currentClient) {
          currentClient = detected;
          cachedSessionStart = null;
          cachedSessionFile = null;
          cachedProjectName = null;
          cachedSessionStats = null;
          cachedModel = null;
          lastActivityHash = null;
          sessionDirty = true;
        }

        // Only rescan session files when fs watcher signals a change
        if (sessionDirty) {
          sessionDirty = false;
          if (detected === 'desktop') {
            // Desktop: don't use Code JSONL files for session timing
            if (!cachedSessionStart) cachedSessionStart = Date.now();
            cachedModel = detectModel(detected, null);
          } else {
            const sessionInfo = getSessionStartTime();
            if (sessionInfo.file !== cachedSessionFile) {
              cachedSessionFile = sessionInfo.file;
              cachedSessionStart = sessionInfo.timestamp;
              cachedProjectName = detectProjectName(cachedSessionFile);
              lastActivityHash = null;
            }
            // Refresh stats and model only on dirty
            cachedSessionStats = getSessionStats(cachedSessionFile);
            cachedModel = detectModel(detected, cachedSessionFile);
          }
        }

        // Desktop: refresh model from watcher every cycle (UI changes don't trigger sessionDirty)
        if (detected === 'desktop' && watcherState.model) {
          const clean = watcherState.model.replace(/\s*Extended\s*/i, '').trim();
          cachedModel = clean;
          if (watcherState.extended) {
            cachedModel += ' Extended';
          }
        }

        // Desktop is never idle while the process is running — JSONL files are Code-only
        const idle = detected === 'desktop' ? false : isSessionIdle(cachedSessionFile);
        const isThinking = !idle && detected === 'code' ? detectThinkingState(cachedSessionFile) : false;
        const activityType = idle ? 'away' : detected;
        const a = buildActivity(activityType, idle ? null : cachedSessionStats, cachedProjectName, isThinking, cachedModel);
        const activityPayload = {
          type: 3,
          name: 'Claude AI',
          details: a.details,
          state: a.state,
          startTimestamp: idle ? null : cachedSessionStart,
          assets: {
            largeImage: a.largeImageKey,
            largeText: a.largeImageText,
            smallImage: a.smallImageKey,
            smallText: a.smallImageText,
          },
          buttons: a.buttons,
        };

        // Hash without volatile data (thinking, stats) to determine critical changes
        const criticalPayload = {
          client: activityType,
          model: cachedModel,
          project: cachedProjectName,
          mode: watcherState.mode,
          startTimestamp: activityPayload.startTimestamp,
        };
        const criticalHash = hashActivity(criticalPayload);
        const fullHash = hashActivity({ ...activityPayload, details: a.details.replace(' (thinking...)', '') });
        const now = Date.now();
        const isCriticalChange = criticalHash !== lastActivityHash;
        const isTimedUpdate = fullHash !== lastActivityHash && (now - lastActivityTime) >= MIN_ACTIVITY_INTERVAL;

        if (isCriticalChange || isTimedUpdate) {
          lastActivityHash = isCriticalChange ? criticalHash : fullHash;
          lastActivityTime = now;
          client.user.setActivity(activityPayload);
        }
      } else {
        if (currentClient !== null) {
          updateStatusLine();
          currentClient = null;
          cachedSessionStart = null;
          cachedSessionFile = null;
          cachedProjectName = null;
          cachedSessionStats = null;
          lastActivityHash = null;
        }

        const a = buildActivity('idle', null, null, false, null);
        const activityPayload = {
          type: 3,
          name: 'Claude AI',
          details: a.details,
          state: a.state,
          assets: {
            largeImage: a.largeImageKey,
            largeText: a.largeImageText,
          },
          buttons: a.buttons,
        };

        const idleHash = hashActivity({ client: 'idle' });
        if (idleHash !== lastActivityHash) {
          lastActivityHash = idleHash;
          lastActivityTime = Date.now();
          client.user.setActivity(activityPayload);
        }
      }
    }

    update();
    setInterval(update, UPDATE_INTERVAL);
  });

  client.on('disconnected', () => {
    rpcConnected = false;
    lastActivityHash = null;
    scheduleReconnect();
  });

  client.on('error', () => {
    if (!rpcConnected) scheduleReconnect();
  });
}

// Wait for first watcher data before connecting to Discord
setTimeout(() => {
  setupClient();
  client.login().catch(() => scheduleReconnect(5000));
}, WATCHER_INTERVAL_MS + 200); // Wait for first watcher output

// Cleanup on exit
function cleanup() {
  if (reconnectTimer) { clearTimeout(reconnectTimer); reconnectTimer = null; }
  stopWatcher(); stopFsWatcher(); releaseLock();
}
process.on('SIGINT', () => { cleanup(); process.exit(0); });
process.on('SIGTERM', () => { cleanup(); process.exit(0); });
process.on('exit', () => { cleanup(); });
