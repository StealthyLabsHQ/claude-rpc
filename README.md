<p align="center"><img src="logo/banner.svg" alt="Claude RPC" width="100%"></p>

# Claude RPC

Lightweight Discord Rich Presence for Claude Code and Claude Desktop on Windows and macOS.

## Features

- Native Tauri/Rust system tray app
- No bundled Node.js, Python, PyInstaller, or sidecar daemon
- Discord IPC implemented in Rust
- Single process app: `Claude RPC`
- Fast 250 ms detection polling for Desktop model and effort switches
- Claude Code and Claude Desktop process detection on Windows and macOS
- Claude Desktop mode detection: Chat, Code, Cowork, Dispatch on Windows; Chat, Code, Cowork on macOS
- Claude Desktop model detection on macOS from Chat/Cowork local storage, Cowork local agent sessions, and Code session fallback
- Claude Desktop Code effort detection on macOS from `ccd-effort-level`
- Claude Code model/project/session timestamp from `~/.claude/projects/*.jsonl`, including `/model` command output
- Resilient model detection: per-session cache plus `~/.claude.json` fallback so the model still shows during long loads or large attachments
- Provider detection from Claude settings, env, API key helpers, `~/.claude.json`, or OAuth credential patterns
- Usage limit display with cached values: 5h, All, Sonnet only, Design
- Optional visibility toggles for provider, effort, session title, and usage limits
- RPC modes: Playing, Watching, Listening, Competing
- Optional Discord buttons in Watching mode
- DND mode to clear Discord activity while detection keeps running
- Dark/System/Light settings window
- Built-in auto-updater: checks signed GitHub releases on startup, notifies in the settings window and tray, installs in place

## Download

Use the latest GitHub release:

- `claude-rpc.exe` - portable app
- `Claude RPC_3.3.0_x64-setup.exe` - Windows installer
- `Claude RPC_3.3.0_aarch64.dmg` - macOS Apple Silicon app
- `claude-rpc-macos-arm64` - macOS portable binary

Once installed, the app keeps itself up to date: it checks the latest signed release on startup and offers to install new versions from the settings window or tray menu.

## Build

Requirements:

- Rust + Cargo
- Node.js only for Tauri CLI during build
- Visual Studio Build Tools on Windows
- Xcode Command Line Tools on macOS

```powershell
npm install
npm test
npm run tauri:build:windows
```

Outputs:

```text
bin\claude-rpc.exe
src-tauri\target\release\bundle\nsis\Claude RPC_3.3.0_x64-setup.exe
```

Build macOS:

```bash
npm install
npm test
npm run tauri:build:macos
```

Outputs:

```text
bin/claude-rpc-macos-arm64
src-tauri/target/release/bundle/macos/Claude RPC.app
src-tauri/target/release/bundle/dmg/Claude RPC_3.3.0_aarch64.dmg
```

## Configuration

Settings are stored at:

```text
%USERPROFILE%\.claude-rpc\config.json
~/.claude-rpc/config.json
```

Example:

```json
{
  "dnd": false,
  "showLimits": true,
  "showLimit5h": true,
  "showLimitAll": true,
  "showLimitSonnet": true,
  "showLimitDesign": true,
  "showProvider": true,
  "showEffort": true,
  "showSessionTitle": true,
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
| Claude Desktop | `claude.exe` process path on Windows, `.app` process path on macOS |
| Claude Desktop mode | `%APPDATA%\Claude\claude_desktop_config.json` + UI Automation on Windows, `~/Library/Application Support/Claude/claude_desktop_config.json` on macOS |
| Claude Desktop model/effort | UI Automation labels on Windows; Chat/Cowork local storage, Cowork local agent sessions, Code session fallback, and `ccd-effort-level` on macOS |
| Claude usage limits | UI Automation on Usage page + `.claude-rpc\limits-cache.json` on Windows, cached values on macOS |
| Claude Code | `claude.exe`/`claude` process path or recent JSONL activity |
| Claude Code model | active JSONL session tail and `/model` command output, then `~\.claude\settings.json`, env vars, and `~\.claude.json` recent usage as fallbacks; cached per session |
| Provider | Claude env/settings, API key helpers, `~/.claude.json` OAuth account, Bedrock, Vertex, or Foundry markers |

## Notes

Claude usage percentages are only available after Claude exposes them on the Usage page. Use `Refresh` in settings to open the Usage page, then Claude RPC caches the latest valid values.

The v3 refactor intentionally removed the legacy Node/Python runtime path. Runtime is now a single lightweight Tauri executable.

## License

ISC
