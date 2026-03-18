"""Minimal Discord IPC client for Rich Presence — no dependencies."""

import json
import os
import struct
import sys
import time

OP_HANDSHAKE = 0
OP_FRAME = 1
MAX_IPC_MESSAGE = 1024 * 1024  # 1MB max message size


class DiscordIPC:
    def __init__(self, client_id):
        self.client_id = client_id
        self._handle = None  # Win32 handle
        self._sock = None    # Unix socket

    def connect(self):
        for i in range(10):
            try:
                self._open_pipe(i)
                self._send(OP_HANDSHAKE, {'v': 1, 'client_id': self.client_id})
                data = self._recv()
                if data and data.get('evt') == 'READY':
                    return
                self.close()
            except Exception:
                self.close()
        raise ConnectionError('Discord IPC pipe not found — is Discord running?')

    def _open_pipe(self, n):
        if sys.platform == 'win32':
            self._open_pipe_win32(n)
        else:
            self._open_pipe_unix(n)

    def _open_pipe_win32(self, n):
        import ctypes
        import ctypes.wintypes
        path = f'\\\\.\\pipe\\discord-ipc-{n}'
        GENERIC_RW = 0x80000000 | 0x40000000
        OPEN_EXISTING = 3
        handle = ctypes.windll.kernel32.CreateFileW(
            path, GENERIC_RW, 0, None, OPEN_EXISTING, 0, None)
        if handle == -1 or handle == 0xFFFFFFFF:
            raise ConnectionError(f'Cannot open pipe {n}')
        self._handle = handle

    def _open_pipe_unix(self, n):
        import socket
        for env in ('XDG_RUNTIME_DIR', 'TMPDIR', 'TMP', 'TEMP'):
            d = os.environ.get(env)
            if d:
                path = os.path.join(d, f'discord-ipc-{n}')
                if os.path.exists(path):
                    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                    sock.connect(path)
                    self._sock = sock
                    return
        path = f'/tmp/discord-ipc-{n}'
        if os.path.exists(path):
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.connect(path)
            self._sock = sock
            return
        raise ConnectionError(f'Pipe {n} not found')

    def _write_bytes(self, data):
        if self._handle is not None:
            import ctypes
            import ctypes.wintypes
            written = ctypes.wintypes.DWORD(0)
            buf = ctypes.create_string_buffer(data)
            ctypes.windll.kernel32.WriteFile(
                self._handle, buf, len(data), ctypes.byref(written), None)
        elif self._sock:
            self._sock.sendall(data)

    def _read_bytes(self, size):
        if self._handle is not None:
            import ctypes
            import ctypes.wintypes
            buf = ctypes.create_string_buffer(size)
            read = ctypes.wintypes.DWORD(0)
            ok = ctypes.windll.kernel32.ReadFile(
                self._handle, buf, size, ctypes.byref(read), None)
            if not ok:
                return b''
            return buf.raw[:read.value]
        elif self._sock:
            data = b''
            while len(data) < size:
                chunk = self._sock.recv(size - len(data))
                if not chunk:
                    break
                data += chunk
            return data
        return b''

    def _send(self, op, payload):
        data = json.dumps(payload).encode('utf-8')
        header = struct.pack('<II', op, len(data))
        self._write_bytes(header + data)

    def _recv(self):
        header = self._read_bytes(8)
        if len(header) < 8:
            return None
        op, length = struct.unpack('<II', header)
        if length > MAX_IPC_MESSAGE:
            raise ValueError(f'IPC message too large: {length}')
        data = self._read_bytes(length)
        return json.loads(data.decode('utf-8')) if data else None

    def set_activity(self, activity):
        payload = {
            'cmd': 'SET_ACTIVITY',
            'args': {
                'pid': os.getpid(),
                'activity': activity,
            },
            'nonce': str(int(time.time() * 1000)),
        }
        self._send(OP_FRAME, payload)
        return self._recv()

    def clear_activity(self):
        payload = {
            'cmd': 'SET_ACTIVITY',
            'args': {'pid': os.getpid()},
            'nonce': str(int(time.time() * 1000)),
        }
        self._send(OP_FRAME, payload)
        return self._recv()

    def close(self):
        if self._handle is not None:
            try:
                import ctypes
                ctypes.windll.kernel32.CloseHandle(self._handle)
            except Exception:
                pass
            self._handle = None
        if self._sock:
            try:
                self._sock.close()
            except Exception:
                pass
            self._sock = None
