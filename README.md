<p align="center">
  <img width="600" alt="Anthropic" src="https://github.com/user-attachments/assets/ccd6d142-7fda-42f0-b461-a6756cdfa9a1" />
</p>
<p align="center">
  <img width="180" alt="Claude Rich Presence" src="https://github.com/user-attachments/assets/b20718e5-8137-4fcd-b018-24fdb14056ed" />
</p>

<h1 align="center">Claude Rich Presence</h1>

<p align="center">
  Discord Rich Presence for Claude AI - displays real-time activity status on your Discord profile.
</p>

---

## Features

- **Auto-detect Claude client**: Claude Code or Claude Desktop
- **Claude Desktop mode**: detects active tab (Chat, Cowork, Code)
- **Live model tracking**: Opus 4.6, Sonnet 4.6, Haiku 4.5, Opus 3, etc.
- **Extended thinking**: shows when Extended mode is enabled
- **1M context detection**: displays (1M) for supported models
- **Session elapsed time**: linked to your actual session
- **Multi-instance**: shows instance count when multiple Claude Code are running
- **Idle timeout**: shows Away after 15 minutes of inactivity (configurable)
- **Do Not Disturb**: hide your presence when you need focus
- **System tray**: Start on boot toggle (Windows)
- **Zero config**: no Discord Application ID needed
- **Webhook notifications**: optional Discord webhook for session events
- **Config file**: persistent preferences at `~/.claude-rpc/config.json`
- **Logging**: debug logs at `~/.claude-rpc/rpc.log`
- **Security hardened**: audit passed, all findings fixed

## Installation

### Option 1 - Standalone exe (recommended)

1. Download `claude-rpc-windows-x64.zip` from the [latest release](https://github.com/StealthyLabsHQ/claude-rpc/releases/latest)
2. Extract the folder
3. Double-click **`claude-rpc.exe`**

No install needed - Node.js is bundled inside.

### Option 2 - From source

Requires [Node.js](https://nodejs.org/) 18+.

```bash
git clone https://github.com/StealthyLabsHQ/claude-rpc.git
cd claude-rpc
npm install
```

**With system tray (Windows):**
```bash
npm start
```

**Console mode (all platforms):**
```bash
npm run start:cli
```

**Or run directly:**
```bash
node --no-deprecation index.js
```

## CLI Flags

```
claude-rpc [options]

  -v, --version    Show version number
  -h, --help       Show help message
  --verbose        Enable verbose console output
  --dnd            Start in Do Not Disturb mode
```

## Configuration

Claude Rich Presence uses a config file at `~/.claude-rpc/config.json`. Create it to customize behavior:

```json
{
  "logoMode": "url",
  "dnd": false,
  "verbose": false,
  "webhookUrl": null
}
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `logoMode` | string | `"url"` | Logo source: `"url"` (GitHub) or `"asset"` (Discord app) |
| `dnd` | boolean | `false` | Do Not Disturb - hides presence |
| `verbose` | boolean | `false` | Verbose console logging |
| `webhookUrl` | string | `null` | Discord webhook URL for session notifications |

## Auto-detection

| Detection | Source |
|-----------|--------|
| **Client** | Process detection (`claude.exe` path) |
| **Desktop mode** | Windows UI Automation (Chat / Cowork / Code) |
| **Desktop model** | Windows UI Automation (model selector button) |
| **Code model** | `~/.claude/settings.json` or session JSONL |
| **Provider** | Environment variables, API key patterns, OAuth credentials |
| **Session time** | First timestamp in the active session JSONL |
| **Multi-instance** | Process count from watcher |

## Platform Support

| Feature | Windows | macOS | Linux |
|---------|---------|-------|-------|
| Claude Code detection | Full | Full | Full |
| Claude Desktop detection | Full (UI Automation) | Basic (AppleScript) | N/A |
| Desktop mode (Chat/Cowork/Code) | Full | Requires accessibility | N/A |
| Desktop model detection | Full | Via JSONL only | N/A |
| System tray | Full (Start on boot) | Terminal mode | Terminal mode |
| Standalone exe | Yes (.exe + bundled Node.js) | Run from source | Run from source |

## Debugging

Logs are written to `~/.claude-rpc/rpc.log` (auto-rotated at 1 MB).

Run with verbose mode for console output:
```bash
node index.js --verbose
```

## Requirements

- **Windows** 10/11 or **macOS** 12+ or **Linux**
- [Discord](https://discord.com/) desktop client running
- [Claude Code](https://claude.ai/code) or [Claude Desktop](https://claude.ai/download)

## License

ISC
