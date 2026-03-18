<img width="1538" height="173" alt="image" src="https://github.com/user-attachments/assets/ccd6d142-7fda-42f0-b461-a6756cdfa9a1" />
<img width="3840" height="2160" alt="image" src="https://github.com/user-attachments/assets/b20718e5-8137-4fcd-b018-24fdb14056ed" />

<h1 align="center">Claude RPC</h1>

<p align="center">
  Discord Rich Presence for Claude AI — displays real-time activity status on your Discord profile.
</p>

---

## Features

- **Auto-detect Claude client**: Claude Code or Claude Desktop
- **Claude Desktop mode**: detects active tab (Chat, Cowork, Code)
- **Live model tracking**: Opus 4.6, Sonnet 4.6, Haiku 4.5, Opus 3, etc.
- **Extended thinking**: shows when Extended mode is enabled
- **1M context detection**: displays (1M) for supported models
- **Session elapsed time**: linked to your actual session
- **Idle timeout**: shows Away after 15 minutes of inactivity
- **System tray**: Start on boot toggle (Windows)
- **Zero config**: no Discord Application ID needed
- **Security hardened**: audit passed, all findings fixed

## Installation

### Option 1 — Standalone exe (recommended)

1. Download `claude-rpc-windows-x64.zip` from the [latest release](https://github.com/StealthyLabsHQ/claude-rpc/releases/latest)
2. Extract the folder
3. Double-click **`claude-rpc.exe`**

No install needed — Node.js is bundled inside.

### Option 2 — From source

Requires [Node.js](https://nodejs.org/) 18+.

```bash
git clone https://github.com/StealthyLabsHQ/claude-rpc.git
cd claude-rpc
npm install
```

**Windows:** Double-click **`start-claude.bat`**

**macOS/Linux:**
```bash
chmod +x start-claude.sh
./start-claude.sh
```

Or run directly:
```bash
node --no-deprecation index.js
```

## Auto-detection

| Detection | Source |
|-----------|--------|
| **Client** | Process detection (`claude.exe` path) |
| **Desktop mode** | Windows UI Automation (Chat / Cowork / Code) |
| **Desktop model** | Windows UI Automation (model selector button) |
| **Code model** | `~/.claude/settings.json` or session JSONL |
| **Provider** | Environment variables, API key patterns, OAuth credentials |
| **Session time** | First timestamp in the active session JSONL |

## Platform Support

| Feature | Windows | macOS |
|---------|---------|-------|
| Claude Code detection | Full | Full |
| Claude Desktop detection | Full (UI Automation) | Basic (AppleScript) |
| Desktop mode (Chat/Cowork/Code) | Full | Requires accessibility |
| Desktop model detection | Full | Via JSONL only |
| System tray | Full (Start on boot) | Terminal mode |
| Standalone exe | Yes (.exe + bundled Node.js) | Run from source |

## Requirements

- **Windows** 10/11 or **macOS** 12+
- [Discord](https://discord.com/) desktop client running
- [Claude Code](https://claude.ai/code) or [Claude Desktop](https://claude.ai/download)

## License

ISC
