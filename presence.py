"""Core Discord Rich Presence logic: detection, activity, session management."""

import json
import os
import re
import sys
import time
import threading
import hashlib
from pathlib import Path

import logging
import psutil

# Log to file for debugging exe builds
LOG_FILE = os.path.join(os.path.expanduser('~'), '.claude-rpc', 'rpc.log')
os.makedirs(os.path.dirname(LOG_FILE), exist_ok=True)
logging.basicConfig(
    filename=LOG_FILE,
    level=logging.INFO,
    format='%(asctime)s %(levelname)s %(message)s',
    datefmt='%H:%M:%S',
)

# --- Constants ---

MAX_JSONL_SCAN_BYTES = 64 * 1024
MAX_STRING_LENGTH = 128
MODELS_1M_DEFAULT = {'opus-4-6', 'opus-4-5'}
LOGO_URL = 'https://raw.githubusercontent.com/StealthyLabsHQ/claude-rpc/refs/heads/main/logo/discord.png'
UPDATE_REPO = 'StealthyLabsHQ/claude-rpc'
IS_WINDOWS = sys.platform == 'win32'
IS_MACOS = sys.platform == 'darwin'

# --- Helpers ---


def sanitize_string(s, max_len=MAX_STRING_LENGTH):
    if not isinstance(s, str):
        return ''
    return re.sub(r'[^\w\s.\-\u2022()]', '', s)[:max_len].strip()


def read_file_tail(filepath, nbytes):
    size = os.path.getsize(filepath)
    if size <= nbytes:
        with open(filepath, 'r', encoding='utf-8', errors='replace') as f:
            return f.read()
    with open(filepath, 'rb') as f:
        f.seek(size - nbytes)
        data = f.read()
    content = data.decode('utf-8', errors='replace')
    nl = content.find('\n')
    return content[nl + 1:] if nl >= 0 else content


def validate_jsonl_entry(entry):
    if not isinstance(entry, dict):
        return None
    ts = entry.get('timestamp')
    if ts and isinstance(ts, str):
        return entry
    snapshot = entry.get('snapshot')
    if isinstance(snapshot, dict) and isinstance(snapshot.get('timestamp'), str):
        return entry
    return None


# --- Detection: Client ---


def detect_client():
    """Returns (client_type, code_instance_count)."""
    desktop = False
    code_count = 0
    for proc in psutil.process_iter(['name', 'exe']):
        try:
            name = (proc.info['name'] or '').lower().replace('.exe', '')
            if name != 'claude':
                continue
            exe = (proc.info['exe'] or '').lower()
            # Claude Desktop: installed via Windows Store (WindowsApps) or AppData
            if 'windowsapps' in exe or 'anthropicclaude' in exe or 'claude desktop' in exe:
                desktop = True
            # Claude Code: installed via npm/cli in .local/bin, node_modules, etc.
            elif '.local' in exe or 'node_modules' in exe or 'npm' in exe or 'nvm' in exe:
                code_count += 1
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            pass

    # macOS detection
    if not desktop and not code_count:
        for proc in psutil.process_iter(['name']):
            try:
                name = proc.info['name'] or ''
                if name == 'Claude':  # macOS app bundle
                    desktop = True
                elif name == 'claude':  # CLI
                    code_count += 1
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                pass

    if desktop:
        return 'desktop', code_count
    if code_count > 0:
        return 'code', code_count
    return None, 0


# --- Detection: Desktop mode/model (Windows UI Automation) ---


def detect_desktop_info():
    """Returns dict with mode, model, extended keys. Windows only via UI Automation."""
    info = {'mode': None, 'model': None, 'extended': False}

    if IS_WINDOWS:
        try:
            import uiautomation as auto
            window = auto.WindowControl(searchDepth=1, Name='Claude')
            if not window.Exists(0, 0):
                return info
            # Mode: RadioButtons (depth ~14 in Electron app)
            for name in ('Chat', 'Cowork', 'Code'):
                rb = window.RadioButtonControl(searchDepth=25, Name=name)
                if rb.Exists(0, 0):
                    try:
                        if rb.GetSelectionItemPattern().IsSelected:
                            info['mode'] = name
                    except Exception:
                        pass
            # Model: Button or Menu starting with Opus/Sonnet/Haiku (depth ~21)
            for ctrl, depth in auto.WalkControl(window, maxDepth=25):
                n = ctrl.Name or ''
                ct = ctrl.ControlTypeName
                if ct in ('ButtonControl', 'MenuControl') and re.match(r'^(Opus|Sonnet|Haiku)', n):
                    info['model'] = n
                    info['extended'] = bool(re.search(r'extended', n, re.I))
                    break
        except ImportError:
            pass
        except Exception:
            pass

    elif IS_MACOS:
        try:
            import subprocess
            script = '''
            tell application "System Events"
                if exists process "Claude" then
                    tell process "Claude"
                        try
                            set radioButtons to radio buttons of first radio group of first window
                            repeat with rb in radioButtons
                                if value of rb is 1 then return name of rb
                            end repeat
                        end try
                    end tell
                end if
            end tell
            return ""
            '''
            result = subprocess.run(['osascript', '-e', script],
                                    capture_output=True, text=True, timeout=3)
            mode = result.stdout.strip()
            if mode in ('Chat', 'Cowork', 'Code'):
                info['mode'] = mode
        except Exception:
            pass

    return info


# --- Detection: Provider (cached) ---

_cached_provider = None


def detect_provider(claude_dir):
    global _cached_provider
    if _cached_provider:
        return _cached_provider

    env = os.environ
    if env.get('CLAUDE_CODE_USE_BEDROCK') in ('1', 'true'):
        _cached_provider = 'Amazon Bedrock'
    elif env.get('CLAUDE_CODE_USE_VERTEX') in ('1', 'true'):
        _cached_provider = 'Google Cloud Vertex'
    elif env.get('CLAUDE_CODE_USE_FOUNDRY') in ('1', 'true'):
        _cached_provider = 'Microsoft Foundry'
    else:
        config_path = os.path.join(claude_dir, 'config.json')
        creds_path = os.path.join(claude_dir, '.credentials.json')
        try:
            if os.path.exists(config_path):
                with open(config_path, 'r', encoding='utf-8') as _f: raw = _f.read()
                if '"sk-ant-' in raw:
                    _cached_provider = 'Anthropic API'
        except Exception:
            pass
        if not _cached_provider:
            try:
                if os.path.exists(creds_path):
                    with open(creds_path, 'r', encoding='utf-8') as _f: raw = _f.read()
                    if '"claudeAiOauth"' in raw:
                        _cached_provider = 'Claude.ai'
            except Exception:
                pass
        if not _cached_provider:
            _cached_provider = 'Anthropic'
    return _cached_provider


# --- Detection: Model ---


def format_model_name(model_id):
    if not model_id or not isinstance(model_id, str):
        return None
    mid = model_id.lower()[:100]

    ver_match = re.search(r'(\d+)[_-](\d+)', mid)
    ver_single = re.search(r'(\d+)', mid) if not ver_match else None
    version = f'{ver_match.group(1)}.{ver_match.group(2)}' if ver_match else (ver_single.group(1) if ver_single else '')

    ctx_match = re.search(r'\[(\d+m)\]', mid, re.I)
    ctx = f' ({ctx_match.group(1).upper()})' if ctx_match else ''

    if not ctx and version:
        family = 'opus' if 'opus' in mid else 'sonnet' if 'sonnet' in mid else 'haiku' if 'haiku' in mid else None
        if family and f'{family}-{version.replace(".", "-")}' in MODELS_1M_DEFAULT:
            ctx = ' (1M)'

    latest = {'opus': '4.6', 'sonnet': '4.6', 'haiku': '4.5'}
    if 'opusplan' in mid:
        return 'Opus Plan / Sonnet 4.6'
    if 'opus' in mid:
        return f'Opus {version or latest["opus"]}{ctx}'.strip()
    if 'sonnet' in mid:
        return f'Sonnet {version or latest["sonnet"]}{ctx}'.strip()
    if 'haiku' in mid:
        return f'Haiku {version or latest["haiku"]}{ctx}'.strip()
    return sanitize_string(model_id)


def detect_model(client_type, session_file, claude_dir):
    # Priority 0: Desktop UI
    if client_type == 'desktop':
        info = detect_desktop_info()
        if info['model']:
            clean = re.sub(r'\s*Extended\s*', '', info['model'], flags=re.I).strip()
            return f'Claude {clean}'

    # Priority 1: settings.json
    try:
        settings_path = os.path.join(claude_dir, 'settings.json')
        if os.path.exists(settings_path):
            settings = json.loads(open(settings_path, 'r', encoding='utf-8').read())
            if isinstance(settings.get('model'), str):
                return format_model_name(settings['model'])
    except Exception:
        pass

    # Priority 2: JSONL session file
    if session_file:
        try:
            tail = read_file_tail(session_file, MAX_JSONL_SCAN_BYTES)
            for line in reversed(tail.splitlines()):
                try:
                    entry = json.loads(line)
                    if entry.get('type') == 'assistant' and entry.get('message', {}).get('model'):
                        return format_model_name(entry['message']['model'])
                except (json.JSONDecodeError, KeyError):
                    pass
        except Exception:
            pass

    # Priority 3: env vars
    for var in ('CLAUDE_MODEL', 'ANTHROPIC_MODEL'):
        if os.environ.get(var):
            return format_model_name(os.environ[var])
    return None


# --- Detection: Project name ---


def detect_project_name(session_file):
    if not session_file:
        return None
    dir_name = os.path.basename(os.path.dirname(session_file))

    # Strip worktree suffix
    wt_idx = dir_name.find('--claude-worktrees-')
    encoded = dir_name[:wt_idx] if wt_idx >= 0 else dir_name

    # Windows: "D--Users-..." / Unix: "-Users-..." or "Users-..."
    win_match = re.match(r'^([a-zA-Z])--(.+)$', encoded)
    if win_match:
        root = win_match.group(1) + ':\\'
        parts = [p for p in win_match.group(2).split('-') if p]
    else:
        unix_match = re.match(r'^-?(.+)$', encoded)
        if not unix_match:
            return None
        root = '/'
        parts = [p for p in unix_match.group(1).split('-') if p]

    cur = root
    i = 0
    while i < len(parts):
        found = False
        for j in range(i, len(parts)):
            name = '-'.join(parts[i:j + 1])
            full = os.path.join(cur, name)
            if os.path.isdir(full):
                cur = full
                i = j + 1
                found = True
                break
        if not found:
            return '-'.join(parts[i:]) or os.path.basename(cur)
    return os.path.basename(cur)


# --- Session scanning ---


def find_latest_jsonl(claude_dir):
    projects_dir = os.path.join(claude_dir, 'projects')
    if not os.path.isdir(projects_dir):
        return None
    latest_file = None
    latest_mtime = 0
    try:
        for d in os.listdir(projects_dir):
            dpath = os.path.join(projects_dir, d)
            if not os.path.isdir(dpath):
                continue
            for f in os.listdir(dpath):
                if f.endswith('.jsonl'):
                    fpath = os.path.join(dpath, f)
                    mtime = os.path.getmtime(fpath)
                    if mtime > latest_mtime:
                        latest_mtime = mtime
                        latest_file = fpath
    except Exception:
        pass
    return latest_file


def get_session_start_time(claude_dir, cached_start=None, cached_file=None):
    try:
        latest = find_latest_jsonl(claude_dir)
        if latest:
            with open(latest, 'r', encoding='utf-8', errors='replace') as f:
                head = f.read(4096)
            for line in head.splitlines():
                if not line.strip():
                    continue
                try:
                    entry = json.loads(line)
                    ts = entry.get('timestamp') or (entry.get('snapshot') or {}).get('timestamp')
                    if isinstance(ts, str):
                        from datetime import datetime, timezone
                        dt = datetime.fromisoformat(ts.replace('Z', '+00:00'))
                        return int(dt.timestamp()), latest
                except (json.JSONDecodeError, ValueError):
                    pass
    except Exception:
        pass
    # Keep cached value if available
    if cached_start and cached_file:
        return cached_start, cached_file
    return int(time.time()), None


def get_session_stats(session_file):
    if not session_file:
        return None
    try:
        tail = read_file_tail(session_file, 256 * 1024)
        edits = cmds = user_msgs = 0
        for line in tail.splitlines():
            if not line:
                continue
            try:
                entry = json.loads(line)
                validated = validate_jsonl_entry(entry)
                if not validated:
                    continue
                if entry.get('type') == 'user':
                    user_msgs += 1
                if entry.get('type') == 'assistant':
                    for block in entry.get('message', {}).get('content', []):
                        if isinstance(block, dict) and block.get('type') == 'tool_use':
                            name = block.get('name', '')
                            if name in ('Edit', 'Write', 'NotebookEdit'):
                                edits += 1
                            if name == 'Bash':
                                cmds += 1
            except (json.JSONDecodeError, KeyError):
                pass
        return {'edits': edits, 'cmds': cmds, 'depth': user_msgs}
    except Exception:
        return None


def detect_thinking_state(session_file):
    if not session_file:
        return False
    try:
        if time.time() - os.path.getmtime(session_file) > 10:
            return False
        tail = read_file_tail(session_file, 4 * 1024)
        lines = [l for l in tail.splitlines() if l.strip()]
        for line in reversed(lines[-3:]):
            try:
                entry = json.loads(line)
                if entry.get('type') == 'assistant':
                    for block in entry.get('message', {}).get('content', []):
                        if isinstance(block, dict) and block.get('type') == 'thinking':
                            return True
            except (json.JSONDecodeError, KeyError):
                pass
    except Exception:
        pass
    return False


def is_session_idle(session_file, timeout_minutes=15):
    if not session_file:
        return True
    try:
        return (time.time() - os.path.getmtime(session_file)) > timeout_minutes * 60
    except Exception:
        return True


# --- File watcher ---


class SessionWatcher:
    def __init__(self, claude_dir):
        self.dirty = True
        self._observer = None
        projects_dir = os.path.join(claude_dir, 'projects')
        if not os.path.isdir(projects_dir):
            return
        try:
            from watchdog.observers import Observer
            from watchdog.events import FileSystemEventHandler

            class Handler(FileSystemEventHandler):
                def __init__(self, watcher):
                    self.watcher = watcher

                def on_modified(self, event):
                    if event.src_path.endswith('.jsonl'):
                        self.watcher.dirty = True

                def on_created(self, event):
                    if event.src_path.endswith('.jsonl'):
                        self.watcher.dirty = True

            self._observer = Observer()
            self._observer.schedule(Handler(self), projects_dir, recursive=True)
            self._observer.daemon = True
            self._observer.start()
        except ImportError:
            pass  # watchdog not installed, always dirty
        except Exception:
            pass

    def stop(self):
        if self._observer:
            self._observer.stop()


# --- Auto-update check ---


def check_for_updates(current_version):
    def _check():
        try:
            import urllib.request
            url = f'https://api.github.com/repos/{UPDATE_REPO}/releases/latest'
            req = urllib.request.Request(url, headers={
                'User-Agent': f'anthropic-rich-presence/{current_version}'
            })
            with urllib.request.urlopen(req, timeout=5) as resp:
                data = json.loads(resp.read())
                latest = data.get('tag_name', '').lstrip('v')
                if latest and latest != current_version:
                    cv = [int(x) for x in current_version.split('.')]
                    lv = [int(x) for x in latest.split('.')]
                    if lv > cv:
                        print(f'\nUpdate available: v{current_version} → v{latest}')
                        print(f'Download: https://github.com/{UPDATE_REPO}/releases/latest')
        except Exception:
            pass
    threading.Thread(target=_check, daemon=True).start()


# --- Build activity payload ---


def build_activity(client_type, stats, project_name, is_thinking, model, code_instances, desktop_mode=None):
    logo_mode = os.environ.get('DISCORD_LOGO_MODE', 'url').lower()
    logo = 'claude_logo' if logo_mode == 'asset' else LOGO_URL

    details = 'Claude Code'
    if client_type == 'code':
        pass
    elif client_type == 'desktop':
        mode = desktop_mode or 'Chat'
        details = f'Claude Desktop ({mode})'
    elif client_type == 'away':
        details = 'Away'
    elif client_type == 'idle':
        details = 'Idle'

    # State
    model_str = model or 'Claude'
    if client_type == 'away':
        state = f'{model_str} | Anthropic · inactive'
    elif client_type == 'idle':
        state = 'No active Claude session'
    else:
        state = f'{model_str} | Anthropic'

    # Buttons
    if client_type == 'desktop':
        buttons = [{'label': 'Claude Desktop', 'url': 'https://claude.ai/download'},
                   {'label': 'GitHub', 'url': f'https://github.com/{UPDATE_REPO}'}]
    else:
        buttons = [{'label': 'Claude', 'url': 'https://claude.ai'},
                   {'label': 'GitHub', 'url': f'https://github.com/{UPDATE_REPO}'}]

    small_image = 'terminal_icon' if client_type in ('code', 'desktop', 'away') else None
    small_text = {
        'code': 'Claude Code CLI',
        'desktop': f'Claude Desktop',
        'away': 'No recent activity',
    }.get(client_type)

    return {
        'details': details,
        'state': state,
        'large_image': logo,
        'large_text': model_str if client_type == 'code' else 'Powered by Anthropic',
        'small_image': small_image,
        'small_text': small_text,
        'buttons': buttons,
    }


# --- Main presence loop ---


def run_presence(dnd_event, stop_event, tray_status=None):
    """Main loop. Runs in a thread. dnd_event.is_set() = DND active."""
    from discord_ipc import DiscordIPC

    # Load config — hardcoded Application ID (shared for all users)
    DEFAULT_CLIENT_ID = '1483898157854363799'
    client_id = os.environ.get('DISCORD_CLIENT_ID', DEFAULT_CLIENT_ID)
    if not re.match(r'^\d{17,20}$', client_id):
        print('DISCORD_CLIENT_ID invalid')
        return

    claude_dir = os.path.abspath(os.path.expanduser(os.environ.get('CLAUDE_DIR_PATH', '~/.claude')))
    idle_timeout = int(os.environ.get('IDLE_TIMEOUT_MINUTES', '15'))
    version = os.environ.get('APP_VERSION', '3.0.0')

    print(f'Provider: {detect_provider(claude_dir)}')
    check_for_updates(version)

    # Connect to Discord
    rpc = DiscordIPC(client_id)
    try:
        rpc.connect()
        logging.info('Discord IPC connected')
    except Exception as e:
        logging.error(f'Failed to connect: {e}')
        print(f'Failed to connect to Discord: {e}')
        if tray_status: tray_status['text'] = 'Disconnected'
        return
    print('Rich Presence ready')
    if tray_status: tray_status['text'] = 'Connected'

    # State
    watcher = SessionWatcher(claude_dir)
    current_client = None
    cached_start = None
    cached_file = None
    cached_project = None
    cached_stats = None
    cached_model = None
    cached_desktop_mode = None
    last_hash = None
    last_update_time = 0
    min_interval = 60

    try:
        while not stop_event.is_set():
            # DND
            if dnd_event.is_set():
                if last_hash != 'dnd':
                    try:
                        rpc.clear_activity()
                    except Exception:
                        pass
                    last_hash = 'dnd'
                    sys.stdout.write('\r\033[2KDo Not Disturb')
                    sys.stdout.flush()
                    if tray_status: tray_status['text'] = 'Do Not Disturb'
                continue

            client_type, code_instances = detect_client()

            if client_type:
                if client_type != current_client:
                    current_client = client_type
                    cached_start = cached_file = cached_project = cached_stats = cached_model = None
                    last_hash = None
                    watcher.dirty = True

                if watcher.dirty:
                    watcher.dirty = False
                    if client_type == 'desktop':
                        # Desktop: don't use Code JSONL files for session timing
                        if not cached_start:
                            cached_start = int(time.time())
                        cached_model = detect_model(client_type, None, claude_dir)
                    else:
                        start, fpath = get_session_start_time(claude_dir, cached_start, cached_file)
                        if fpath != cached_file:
                            cached_file = fpath
                            cached_start = start
                            cached_project = detect_project_name(cached_file)
                            last_hash = None
                        cached_stats = get_session_stats(cached_file)
                        cached_model = detect_model(client_type, cached_file, claude_dir)

                # Refresh desktop mode on every cycle (live UI state)
                if client_type == 'desktop':
                    info = detect_desktop_info()
                    cached_desktop_mode = info.get('mode')
                    if info.get('model'):
                        raw = info['model']
                        # Strip "Extended" for model name, add back as suffix
                        clean = re.sub(r'\s*Extended\s*', '', raw, flags=re.I).strip()
                        cached_model = format_model_name(clean.lower().replace(' ', '-').replace('.', '-'))
                        if info.get('extended') and cached_model and 'Extended' not in cached_model:
                            cached_model += ' Extended'

                # Desktop is never idle while the process is running — JSONL files are Code-only
                idle = False if client_type == 'desktop' else is_session_idle(cached_file, idle_timeout)
                thinking = not idle and client_type == 'code' and detect_thinking_state(cached_file)
                act_type = 'away' if idle else client_type
                activity = build_activity(act_type, None if idle else cached_stats,
                                          cached_project, thinking, cached_model, code_instances,
                                          cached_desktop_mode)
                ts = None if idle else cached_start

                # Status line
                model_str = cached_model or 'auto-detect'
                proj_part = f' \u2022 {cached_project}' if client_type == 'code' and cached_project else ''
                inst_part = f' [{code_instances}]' if client_type == 'code' and code_instances > 1 else ''
                client_name = {'code': 'Claude Code', 'desktop': 'Claude Desktop'}.get(client_type, 'Idle')
                status_line = f'{client_name}{proj_part}{inst_part} \u2022 {model_str}'
                sys.stdout.write(f'\r\033[2K{status_line}')
                sys.stdout.flush()
                if tray_status: tray_status['text'] = status_line

                # Hash for dedup (exclude thinking to avoid timer reset)
                critical = json.dumps({'c': act_type, 'm': cached_model, 'p': cached_project, 't': ts, 'mode': cached_desktop_mode})
                full = json.dumps({**activity, 'ts': ts, 'details': activity['details'].replace(' (thinking...)', '')})
                now = time.time()
                is_critical = critical != last_hash
                is_timed = full != last_hash and (now - last_update_time) >= min_interval

                if is_critical or is_timed:
                    try:
                        act = {'details': activity['details'], 'state': activity['state'],
                               'assets': {'large_image': activity['large_image'], 'large_text': activity['large_text']},
                               'buttons': activity['buttons']}
                        if activity.get('small_image'):
                            act['assets']['small_image'] = activity['small_image']
                            act['assets']['small_text'] = activity['small_text']
                        if ts:
                            act['timestamps'] = {'start': ts}
                        result = rpc.set_activity(act)
                        logging.info(f'Activity set: {activity["details"]} | {activity["state"]} -> {result and result.get("evt")}')
                        last_hash = critical if is_critical else full
                        last_update_time = now
                    except Exception as e:
                        logging.error(f'Activity failed: {e}')
                        try:
                            rpc.connect()
                        except Exception:
                            pass

            else:
                if current_client is not None:
                    current_client = None
                    cached_start = cached_file = cached_project = cached_stats = cached_model = None
                    sys.stdout.write('\r\033[2KIdle')
                    sys.stdout.flush()
                    if tray_status: tray_status['text'] = 'Idle'

                idle_hash = 'idle'
                if last_hash != idle_hash:
                    activity = build_activity('idle', None, None, False, None, 0)
                    try:
                        act = {'details': activity['details'], 'state': activity['state'],
                               'assets': {'large_image': activity['large_image'], 'large_text': activity['large_text']},
                               'buttons': activity['buttons']}
                        rpc.set_activity(act)
                    except Exception:
                        try:
                            rpc.connect()
                        except Exception:
                            pass
                    last_hash = idle_hash
                    last_update_time = time.time()

            time.sleep(3)

    finally:
        watcher.stop()
        try:
            rpc.close()
        except Exception:
            pass
