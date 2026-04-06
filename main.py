"""Claude RPC - Tray wrapper that launches Node.js RPC from runtime/."""

import os
import signal
import subprocess
import sys
import threading
import atexit
from pathlib import Path

# --- Single instance (Named Mutex on Windows, lock file on Unix) ---

_mutex_handle = None  # keep reference so GC doesn't close it


def acquire_single_instance():
    """Block second instances from running. Must be called at startup."""
    global _mutex_handle
    if sys.platform == 'win32':
        import ctypes
        ERROR_ALREADY_EXISTS = 183
        _mutex_handle = ctypes.windll.kernel32.CreateMutexW(
            None, True, 'Global\\ClaudeRPC_SingleInstance')
        if ctypes.windll.kernel32.GetLastError() == ERROR_ALREADY_EXISTS:
            sys.exit(0)  # silent exit - another instance owns the mutex
    else:
        # Unix fallback: lock file
        lock_dir = os.path.join(os.path.expanduser('~'), '.claude-rpc')
        lock_file = os.path.join(lock_dir, 'rpc.lock')
        os.makedirs(lock_dir, mode=0o700, exist_ok=True)
        if os.path.exists(lock_file):
            try:
                pid = int(open(lock_file).read().strip())
                os.kill(pid, 0)
                sys.exit(0)
            except (ValueError, OSError):
                pass
        with open(lock_file, 'w') as f:
            f.write(str(os.getpid()))
        atexit.register(lambda: os.unlink(lock_file) if os.path.exists(lock_file) else None)


def release_lock():
    pass  # mutex released automatically when process exits


# --- Node.js subprocess ---

node_process = None
tray_status = {'text': 'Starting...'}


def get_exe_dir():
    """Directory of the running exe (or script dir in dev mode)."""
    if getattr(sys, 'frozen', False):
        return os.path.dirname(sys.executable)
    return os.path.dirname(os.path.abspath(__file__))


def get_resource_dir():
    """Where embedded resources live: _MEIPASS in onefile builds, exe_dir otherwise."""
    if getattr(sys, 'frozen', False) and hasattr(sys, '_MEIPASS'):
        return sys._MEIPASS
    return get_exe_dir()


def find_node():
    """Find node.exe: embedded resources first, then system PATH."""
    res_dir = get_resource_dir()
    exe_dir = get_exe_dir()
    for candidate in [
        os.path.join(res_dir, 'runtime', 'node.exe'),
        os.path.join(exe_dir, 'runtime', 'node.exe'),
        os.path.join(exe_dir, 'node.exe'),
    ]:
        if os.path.exists(candidate):
            return candidate
    import shutil
    return shutil.which('node')


def find_index_js():
    """Find index.js: embedded resources first, then exe dir."""
    res_dir = get_resource_dir()
    exe_dir = get_exe_dir()
    for candidate in [
        os.path.join(res_dir, 'runtime', 'index.js'),
        os.path.join(exe_dir, 'runtime', 'index.js'),
        os.path.join(exe_dir, 'index.js'),
        os.path.join(os.path.dirname(os.path.abspath(__file__)), 'index.js'),
    ]:
        if os.path.exists(candidate):
            return candidate
    return None


def _bind_child_to_job(pid):
    """Kill child process when parent exits (Windows Job Object)."""
    import ctypes
    from ctypes import wintypes

    kernel32 = ctypes.windll.kernel32
    PROCESS_SET_QUOTA = 0x0100
    PROCESS_TERMINATE = 0x0001
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x2000
    JobObjectExtendedLimitInformation = 9

    class JOBOBJECT_BASIC_LIMIT_INFORMATION(ctypes.Structure):
        _fields_ = [
            ('PerProcessUserTimeLimit', ctypes.c_int64),
            ('PerJobUserTimeLimit', ctypes.c_int64),
            ('LimitFlags', wintypes.DWORD),
            ('MinimumWorkingSetSize', ctypes.c_size_t),
            ('MaximumWorkingSetSize', ctypes.c_size_t),
            ('ActiveProcessLimit', wintypes.DWORD),
            ('Affinity', ctypes.POINTER(ctypes.c_ulong)),
            ('PriorityClass', wintypes.DWORD),
            ('SchedulingClass', wintypes.DWORD),
        ]

    class IO_COUNTERS(ctypes.Structure):
        _fields_ = [
            ('ReadOperationCount', ctypes.c_uint64),
            ('WriteOperationCount', ctypes.c_uint64),
            ('OtherOperationCount', ctypes.c_uint64),
            ('ReadTransferCount', ctypes.c_uint64),
            ('WriteTransferCount', ctypes.c_uint64),
            ('OtherTransferCount', ctypes.c_uint64),
        ]

    class JOBOBJECT_EXTENDED_LIMIT_INFORMATION(ctypes.Structure):
        _fields_ = [
            ('BasicLimitInformation', JOBOBJECT_BASIC_LIMIT_INFORMATION),
            ('IoInfo', IO_COUNTERS),
            ('ProcessMemoryLimit', ctypes.c_size_t),
            ('JobMemoryLimit', ctypes.c_size_t),
            ('PeakProcessMemoryUsed', ctypes.c_size_t),
            ('PeakJobMemoryUsed', ctypes.c_size_t),
        ]

    job = kernel32.CreateJobObjectW(None, None)
    if not job:
        return
    info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION()
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    kernel32.SetInformationJobObject(
        job, JobObjectExtendedLimitInformation,
        ctypes.byref(info), ctypes.sizeof(info))
    handle = kernel32.OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, False, pid)
    if handle:
        kernel32.AssignProcessToJobObject(job, handle)
        kernel32.CloseHandle(handle)
    global _job_handle
    _job_handle = job


def start_node():
    global node_process
    node = find_node()
    index_js = find_index_js()

    if not node:
        tray_status['text'] = 'Error: Node.js not found'
        print('Error: node.exe not found - put node.exe in the runtime/ folder')
        return
    if not index_js:
        tray_status['text'] = 'Error: index.js not found'
        print('Error: index.js not found in runtime/')
        return

    env = os.environ.copy()
    # Set NODE_PATH so node finds node_modules regardless of PyInstaller extraction nesting
    _nm = os.path.join(get_resource_dir(), 'runtime', 'node_modules')
    # PyInstaller may extract to a doubled path: node_modules/node_modules/
    _nm_nested = os.path.join(_nm, 'node_modules')
    node_modules_path = _nm_nested if os.path.isdir(_nm_nested) else _nm
    if os.path.isdir(node_modules_path):
        existing = env.get('NODE_PATH', '')
        env['NODE_PATH'] = node_modules_path + (os.pathsep + existing if existing else '')
    # Load .env — check next to EXE first (user config), then embedded runtime/
    _env_candidates = [
        os.path.join(get_exe_dir(), '.env'),
        os.path.join(get_resource_dir(), 'runtime', '.env'),
        os.path.join(os.path.dirname(index_js), '.env'),
    ]
    env_file = next((p for p in _env_candidates if os.path.exists(p)), None)
    if env_file and os.path.exists(env_file):
        with open(env_file, 'r', encoding='utf-8') as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith('#') and '=' in line:
                    k, _, v = line.partition('=')
                    env.setdefault(k.strip(), v.strip())

    node_process = subprocess.Popen(
        [node, '--no-deprecation', index_js],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        cwd=os.path.dirname(index_js),
        env=env,
        creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == 'win32' else 0,
    )

    if sys.platform == 'win32':
        try:
            _bind_child_to_job(node_process.pid)
        except Exception:
            pass

    def read_output():
        for line in iter(node_process.stdout.readline, b''):
            text = line.decode('utf-8', errors='replace').strip()
            if not text:
                continue
            clean = text.replace('\r', '').replace('\x1b[2K', '').strip()
            if clean:
                tray_status['text'] = clean
                print(clean)

    threading.Thread(target=read_output, daemon=True).start()


def stop_node():
    global node_process
    if node_process:
        try:
            node_process.terminate()
            node_process.wait(timeout=3)
        except Exception:
            try:
                node_process.kill()
            except Exception:
                pass
        node_process = None


# --- System tray ---

IS_WINDOWS = sys.platform == 'win32'


def get_icon_path():
    res_dir = get_resource_dir()
    exe_dir = get_exe_dir()
    for p in [
        os.path.join(res_dir, 'logo', 'tray-icon.png'),
        os.path.join(exe_dir, 'logo', 'tray-icon.png'),
    ]:
        if os.path.exists(p):
            return p
    return None


def start_with_tray(stop_event):
    try:
        import pystray
        from PIL import Image
    except ImportError:
        print('pystray/Pillow not installed. Running without tray.')
        return False

    icon_path = get_icon_path()
    if not icon_path:
        print('Tray icon not found. Running without tray.')
        return False

    image = Image.open(icon_path)

    def on_quit(icon, item):
        stop_event.set()
        stop_node()
        icon.stop()

    def status_text(item):
        return tray_status['text']

    menu = pystray.Menu(
        pystray.MenuItem('Claude RPC', None, enabled=False),
        pystray.MenuItem(status_text, None, enabled=False),
        pystray.Menu.SEPARATOR,
        pystray.MenuItem('Start on boot', on_boot, checked=lambda item: _is_startup_enabled()),
        pystray.Menu.SEPARATOR,
        pystray.MenuItem('Quit', on_quit),
    )

    icon = pystray.Icon('claude-rpc', image, 'Claude RPC', menu)

    def update_tooltip():
        while not stop_event.is_set():
            icon.title = f'Claude RPC - {tray_status["text"]}'
            try:
                icon.update_menu()
            except Exception:
                pass
            stop_event.wait(5)

    threading.Thread(target=update_tooltip, daemon=True).start()
    print('Tray icon ready')
    icon.run()
    return True


# --- Start on boot ---

def _get_exe_command():
    if getattr(sys, 'frozen', False):
        return f'"{sys.executable}"'
    return f'"{sys.executable}" "{os.path.abspath(__file__)}"'


def _is_startup_enabled():
    if IS_WINDOWS:
        try:
            import winreg
            key = winreg.OpenKey(winreg.HKEY_CURRENT_USER,
                                 r'Software\Microsoft\Windows\CurrentVersion\Run',
                                 0, winreg.KEY_READ)
            winreg.QueryValueEx(key, 'ClaudeRPC')
            winreg.CloseKey(key)
            return True
        except (FileNotFoundError, OSError):
            return False
    elif sys.platform == 'darwin':
        return os.path.exists(os.path.expanduser(
            '~/Library/LaunchAgents/com.stealthylabs.claude-rpc.plist'))
    return False


def on_boot(icon, item):
    if _is_startup_enabled():
        _disable_startup()
    else:
        _enable_startup()


def _enable_startup():
    cmd = _get_exe_command()
    if IS_WINDOWS:
        try:
            import winreg
            key = winreg.OpenKey(winreg.HKEY_CURRENT_USER,
                                 r'Software\Microsoft\Windows\CurrentVersion\Run',
                                 0, winreg.KEY_SET_VALUE)
            winreg.SetValueEx(key, 'ClaudeRPC', 0, winreg.REG_SZ, cmd)
            winreg.CloseKey(key)
        except Exception:
            pass
    elif sys.platform == 'darwin':
        from xml.sax.saxutils import escape
        plist_path = os.path.expanduser(
            '~/Library/LaunchAgents/com.stealthylabs.claude-rpc.plist')
        plist = f'''<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.stealthylabs.claude-rpc</string>
    <key>ProgramArguments</key><array><string>{escape(sys.executable)}</string><string>{escape(os.path.abspath(__file__))}</string></array>
    <key>RunAtLoad</key><true/>
</dict>
</plist>'''
        os.makedirs(os.path.dirname(plist_path), exist_ok=True)
        with open(plist_path, 'w') as f:
            f.write(plist)


def _disable_startup():
    if IS_WINDOWS:
        try:
            import winreg
            key = winreg.OpenKey(winreg.HKEY_CURRENT_USER,
                                 r'Software\Microsoft\Windows\CurrentVersion\Run',
                                 0, winreg.KEY_SET_VALUE)
            winreg.DeleteValue(key, 'ClaudeRPC')
            winreg.CloseKey(key)
        except Exception:
            pass
    elif sys.platform == 'darwin':
        plist = os.path.expanduser(
            '~/Library/LaunchAgents/com.stealthylabs.claude-rpc.plist')
        if os.path.exists(plist):
            os.unlink(plist)


# --- Entry point ---

def main():
    acquire_single_instance()
    atexit.register(release_lock)
    atexit.register(stop_node)

    stop_event = threading.Event()

    signal.signal(signal.SIGINT, lambda s, f: (stop_event.set(), stop_node(), sys.exit(0)))
    signal.signal(signal.SIGTERM, lambda s, f: (stop_event.set(), stop_node(), sys.exit(0)))

    if sys.platform == 'win32':
        try:
            import ctypes
            CTRL_CLOSE_EVENT = 2
            CTRL_LOGOFF_EVENT = 5
            CTRL_SHUTDOWN_EVENT = 6

            @ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_uint)
            def console_handler(event):
                if event in (CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT):
                    stop_event.set()
                    stop_node()
                return False

            ctypes.windll.kernel32.SetConsoleCtrlHandler(console_handler, True)
            main._console_handler = console_handler
        except Exception:
            pass

    start_node()

    if not start_with_tray(stop_event):
        try:
            while not stop_event.is_set() and node_process and node_process.poll() is None:
                stop_event.wait(1)
        except KeyboardInterrupt:
            pass
        finally:
            stop_node()


if __name__ == '__main__':
    if sys.platform == 'win32':
        import multiprocessing
        multiprocessing.freeze_support()
    main()
