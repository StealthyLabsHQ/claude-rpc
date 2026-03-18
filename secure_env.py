"""Secure .env storage using keyring (Windows Credential Manager / macOS Keychain)."""

import base64
import os
import sys

try:
    import keyring
except ImportError:
    keyring = None

SERVICE = 'AnthropicRichPresence'
ACCOUNT = 'env'
ENV_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), '.env')


def encrypt():
    if not os.path.exists(ENV_PATH):
        print('.env file not found')
        sys.exit(1)
    if not keyring:
        print('keyring not installed: pip install keyring')
        sys.exit(1)
    with open(ENV_PATH, 'r', encoding='utf-8') as f:
        content = f.read()
    b64 = base64.b64encode(content.encode('utf-8')).decode('ascii')
    keyring.set_password(SERVICE, ACCOUNT, b64)
    user = os.environ.get('USER') or os.environ.get('USERNAME', 'unknown')
    print(f'Encrypted .env -> keyring ({SERVICE}, tied to {user})')
    print('You can now delete .env if desired.')


def decrypt():
    if not keyring:
        return None
    try:
        b64 = keyring.get_password(SERVICE, ACCOUNT)
        if not b64:
            return None
        return base64.b64decode(b64).decode('utf-8')
    except Exception as e:
        print(f'Failed to decrypt from keyring: {e}', file=sys.stderr)
        return None


def load_secure_env():
    content = decrypt()
    if not content:
        return False
    for line in content.splitlines():
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        eq = line.find('=')
        if eq < 0:
            continue
        key = line[:eq].strip()
        value = line[eq + 1:].strip()
        if key not in os.environ:
            os.environ[key] = value
    return True


if __name__ == '__main__':
    cmd = sys.argv[1] if len(sys.argv) > 1 else ''
    if cmd == 'encrypt':
        encrypt()
    elif cmd == 'decrypt':
        content = decrypt()
        if content:
            print('[WARNING] Sensitive data follows:', file=sys.stderr)
            print(content)
        else:
            print('No credentials found or decryption failed')
    else:
        print('Usage: python secure_env.py <encrypt|decrypt>')
