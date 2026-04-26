# Claude RPC

Lightweight Discord Rich Presence for Claude Code and Claude Desktop.

## Features

- Native Tauri/Rust system tray app
- No bundled Node.js, Python, PyInstaller, or sidecar daemon
- Discord IPC implemented in Rust
- Single process in Task Manager: `Claude RPC`
- Claude Code and Claude Desktop process detection
- Claude Desktop mode detection: Chat, Code, Cowork, Dispatch
- Claude Code model/project/session timestamp from `~/.claude/projects/*.jsonl`
- Provider detection from Claude settings, env, API key, or OAuth credential patterns
- Usage limit display with cached values: 5h, All, Sonnet only, Design
- Optional visibility toggles for provider, effort, and usage limits
- RPC modes: Playing, Watching, Listening, Competing
- Optional Discord buttons in Watching mode
- DND mode to clear Discord activity while detection keeps running
- Dark/System/Light settings window

## Download

Use the latest GitHub release:

- `claude-rpc.exe` - portable app
- `Claude RPC_3.0.1_x64-setup.exe` - Windows installer

## Build

Requirements:

- Rust + Cargo
- Node.js only for Tauri CLI during build
- Visual Studio Build Tools on Windows

```powershell
npm install
npm test
npm run build
```

Outputs:

```text
bin\claude-rpc.exe
src-tauri\target\release\bundle\nsis\Claude RPC_3.0.1_x64-setup.exe
```

## Configuration

Settings are stored at:

```text
%USERPROFILE%\.claude-rpc\config.json
```

Example:

```json
{
  "logoMode": "url",
  "dnd": false,
  "showLimits": true,
  "showLimit5h": true,
  "showLimitAll": true,
  "showLimitSonnet": true,
  "showLimitDesign": true,
  "showProvider": true,
  "showEffort": true,
  "rpcMode": "watching",
  "buttons": [
    { "label": "Claude", "url": "https://claude.ai" },
    { "label": "GitHub Repo", "url": "https://github.com/StealthyLabsHQ/claude-rpc" }
  ]
}
```

## Detection

| Target | Method |
|---|---|
| Claude Desktop | `claude.exe` process path |
| Claude Desktop mode | `%APPDATA%\Claude\claude_desktop_config.json` + UI Automation |
| Claude Desktop model/effort | UI Automation labels |
| Claude usage limits | UI Automation on Usage page + `.claude-rpc\limits-cache.json` |
| Claude Code | `claude.exe` process path or recent JSONL activity |
| Claude Code model | `~\.claude\settings.json` or JSONL session tail |
| Provider | env/settings/API key/OAuth credential patterns |

## Notes

Claude usage percentages are only available after Claude exposes them on the Usage page. Use `Refresh` in settings to open the Usage page, then Claude RPC caches the latest valid values.

The v3 refactor intentionally removed the legacy Node/Python runtime path. Runtime is now a single lightweight Tauri executable.

## License

ISC
