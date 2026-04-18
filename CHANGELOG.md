# Changelog

## v2.3.4 (2026-04-18)

### Added
- **Claude Desktop Dispatch submode** detection via UI Automation scoring (e.g. `Cowork - Dispatch`)
- **Adaptive / Extended thinking** detection via `TogglePattern` — checks element, parent, and children so toggle state isn't inferred from label presence alone
- **Effort level** display (Low / Medium / High / Extra high / Max) for both:
  - Claude Desktop (parsed from UI button labels, e.g. `Sonnet 4.6 · High`)
  - Claude Code CLI (read from `~/.claude/settings.json` `effortLevel` field)
- **Provider expansion** — `detectProvider()` now also reads `~/.claude/settings.json` `env` block, supporting:
  - Anthropic API
  - Claude Account
  - Amazon Bedrock (`CLAUDE_CODE_USE_BEDROCK`)
  - Google GCP Vertex (`CLAUDE_CODE_USE_VERTEX`)
  - Microsoft Foundry (`CLAUDE_CODE_USE_FOUNDRY`)

### Changed
- **Tray menu redesign** (Codex-style layout):
  ```
  Claude Rich Presence
  Claude: Off / CLI (Code) / Desktop (Chat | Cowork | Cowork - Dispatch | Code)
  Claude Sonnet 4.6 · Extra high
  Provider: Anthropic API
  Discord: Connected
  ```
- Model line no longer carries `Model:` prefix (matches Codex Rich Presence style)
- Faster refresh intervals for Discord Rich Presence updates

### Fixed
- **`cachedModel` bakes in effort suffix** — effort is now re-read each tick, so `/effort medium` updates Discord within seconds without needing a session/model restart
- **PowerShell regex middle dot (`·`)** — `\u00b7` escape fixes effort extraction under Windows-1252 decode
- **`Sort-Object` on hashtables** — now uses a script block `{ $_.Score }`; sorting by property name silently ignored hashtable keys and broke mode scoring
- **Adaptive/Extended leaks between modes** — `watcherState` now resets `adaptive`/`extended` when mode or model changes

### Build
- `requirements.txt` bumped for Python 3.14: `pyinstaller>=6.15.0`, `Pillow>=11.0.0`
- `build.bat` uses `call` prefix for `.cmd` shims (npm, pip, pyinstaller) so the outer batch doesn't exit early

## v2.3.0 (2026-04-07)

### Added
- **All-in-one exe** — single `claude-rpc.exe` (~47 MB) embeds node.exe, JS runtime, node_modules, and logo assets. No external folders needed — double-click and go.
- **`scripts/build-dist.js`** — local build script matching the release CI pipeline
- **`launcher.js`** — experimental pure Node.js launcher with single-instance lock (Windows named pipe), not used in default build
- **`logo/tray-icon.b64`** — base64-encoded PNG tray icon source file
- **`sea-config.json`** + `build:sea` npm script — experimental Node.js Single Executable Application support

### Fixed
- **Zero console window** — PyInstaller `--windowed` (GUI subsystem) + `node.exe` with `CREATE_NO_WINDOW` ensures no CMD/PowerShell flash on launch
- **Tray process leak** — SIGINT/SIGTERM now explicitly kill the PowerShell tray before exit

### Changed
- **Release artifact** — single exe replaces the previous zip archive (exe + runtime/ + logo/)
- Tray icon loaded from `logo/tray-icon.b64` at runtime instead of inline base64 constant
- `.gitignore` now excludes `.claude/` local settings directory

## v2.2.1 (2026-04-06)

### Security
- **Pin Python deps to exact versions** - `requirements.txt` switched from `>=` to `==` for all 5 deps (`pystray`, `Pillow`, `python-dotenv`, `watchdog`, `uiautomation`), eliminating the supply-chain risk of unpinned PyPI resolution at release build time

### Removed
- `psutil` and `keyring` from `requirements.txt` - orphaned by removal of `presence.py` and `secure_env.py`

### Fixed
- `version_info.txt` version corrected to `2.2.0.0` (was incorrectly set to `2.1.0.0`)

## v2.2.0 (2026-04-04)

### Added
- **Config file** (`~/.claude-rpc/config.json`) for persistent preferences (idle timeout, DND, logo mode, webhook, verbose)
- **CLI flags**: `--version`, `--help`, `--verbose`, `--dnd`, `--no-idle`
- **Do Not Disturb mode** via config or `--dnd` flag - hides Discord presence
- **File-based logging** at `~/.claude-rpc/rpc.log` with automatic 1 MB rotation
- **Linux support** in watcher script (Claude Code detection via pgrep)
- **Multi-instance display** - shows instance count when multiple Claude Code sessions are running
- **Discord webhook notifications** (optional) on session start/end/away events
- **System tray for Node.js** (`tray.js`) - Windows NotifyIcon with DND toggle, Start on Boot, Quit
- **Automated CI/CD** - GitHub Actions for testing (Node 18/20/22) and release builds
- **Test suite** - 20 tests covering formatModelName, compareVersions, sanitizeString, config
- **Status file** (`~/.claude-rpc/status.txt`) for tray communication

### Fixed
- **LOGO_URL** pointed to old repo name `anthropic-rich-presence` instead of `claude-rpc`
- **findLatestJsonlFile()** now scans recursively (depth-limited to 3, excludes node_modules/.git/.venv)
- **Provider cache** now expires after 5 minutes instead of being permanent
- **DND mode** was referencing `global.dndMode` which was never set (dead code)
- **Idle timeout** now configurable via config file (was hardcoded to env var only)
- **.env loading** now checks `__dirname` first, fixing "DISCORD_CLIENT_ID missing" errors
- **Duplicate `atexit` import** in main.py
- **Build script** now installs production-only dependencies (`--omit=dev`), saving ~50 MB

### Removed
- `presence.py` (737 lines) - legacy RPC logic fully replaced by index.js
- `discord_ipc.py` (152 lines) - replaced by @xhayper/discord-rpc
- `secure_env.py` (77 lines) - replaced by secure-env.js
- `anthropic-rich-presence.spec` - legacy PyInstaller spec

### Changed
- `index.js` refactored to export `start()` function (importable by tray.js without side effects)
- `package.json` updated: main points to index.js, added vitest, keywords, engines, files field
- `.gitignore` cleaned up: added .venv, *.log, IDE dirs
- Watcher script bumped to v11 with Linux support
- Release workflow now builds `claude-rpc.exe` via PyInstaller + bundles Node.js runtime

## v2.1.0 (2026-03-31)

### Added
- Display "Opus Plan / Sonnet 4.6" in Discord RPC for Opus Plan Mode

## v2.0.0 (2026-03-20)

### Added
- Initial release
- Auto-detect Claude Code and Claude Desktop
- Live model tracking (Opus, Sonnet, Haiku)
- Extended thinking detection
- 1M context badge for supported models
- Session elapsed time from JSONL timestamps
- Idle timeout (15 minutes)
- Windows system tray with Start on Boot
- Zero-config Discord Application ID
- DPAPI/Keychain credential encryption
