// Secure .env storage - DPAPI on Windows, Keychain on macOS
// Encrypts .env values tied to the current user account

const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

const IS_MACOS = process.platform === 'darwin';
const ENV_PATH = path.join(__dirname, '.env');
const SECURE_PATH = path.join(__dirname, '.env.encrypted');
const KEYCHAIN_SERVICE = 'AnthropicRichPresence';
const KEYCHAIN_ACCOUNT = 'env';

// --- Platform-specific encrypt/decrypt (no shell interpolation) ---

function platformEncrypt(plaintext) {
  const b64 = Buffer.from(plaintext, 'utf8').toString('base64');

  if (IS_MACOS) {
    try { execFileSync('security', ['delete-generic-password', '-s', KEYCHAIN_SERVICE, '-a', KEYCHAIN_ACCOUNT], { encoding: 'utf8', stdio: 'ignore' }); } catch {}
    execFileSync('security', ['add-generic-password', '-s', KEYCHAIN_SERVICE, '-a', KEYCHAIN_ACCOUNT, '-w', b64], { encoding: 'utf8' });
    return 'keychain';
  } else {
    const ps = `$ss = ConvertTo-SecureString -String '${b64}' -AsPlainText -Force; ConvertFrom-SecureString $ss`;
    return execFileSync('powershell', ['-NoProfile', '-Command', ps], { encoding: 'utf8' }).trim();
  }
}

function platformDecrypt(encrypted) {
  if (IS_MACOS || encrypted === 'keychain') {
    const b64 = execFileSync('security', ['find-generic-password', '-s', KEYCHAIN_SERVICE, '-a', KEYCHAIN_ACCOUNT, '-w'], { encoding: 'utf8' }).trim();
    return Buffer.from(b64, 'base64').toString('utf8');
  } else {
    const cleaned = encrypted.replace(/\s/g, '');
    if (!/^[0-9a-fA-F]+$/.test(cleaned)) {
      throw new Error('Invalid encrypted data format');
    }
    const ps = `$ss = ConvertTo-SecureString -String '${cleaned}'; $ptr = [System.Runtime.InteropServices.Marshal]::SecureStringToBSTR($ss); $b64 = [System.Runtime.InteropServices.Marshal]::PtrToStringAuto($ptr); [System.Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr); [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b64))`;
    return execFileSync('powershell', ['-NoProfile', '-Command', ps], { encoding: 'utf8' }).trim();
  }
}

// --- Shared logic ---

function encrypt() {
  if (!fs.existsSync(ENV_PATH)) {
    console.error('.env file not found');
    process.exit(1);
  }
  const content = fs.readFileSync(ENV_PATH, 'utf8');
  const encrypted = platformEncrypt(content);
  fs.writeFileSync(SECURE_PATH, encrypted, { encoding: 'utf8', mode: 0o600 });
  const backend = IS_MACOS ? 'Keychain' : 'DPAPI';
  const user = process.env.USER || process.env.USERNAME;
  console.log(`Encrypted .env -> .env.encrypted (${backend}, tied to ${user})`);
  console.log('You can now delete .env if desired.');
}

function decrypt() {
  if (!fs.existsSync(SECURE_PATH)) return null;
  try {
    const encrypted = fs.readFileSync(SECURE_PATH, 'utf8').trim();
    return platformDecrypt(encrypted);
  } catch (e) {
    console.error('Failed to decrypt .env.encrypted:', e.message);
    return null;
  }
}

function loadSecureEnv() {
  const decrypted = decrypt();
  if (!decrypted) return false;
  for (const line of decrypted.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;
    const eqIdx = trimmed.indexOf('=');
    if (eqIdx < 0) continue;
    const key = trimmed.slice(0, eqIdx).trim();
    const value = trimmed.slice(eqIdx + 1).trim();
    if (!process.env[key]) {
      process.env[key] = value;
    }
  }
  return true;
}

if (require.main === module) {
  const cmd = process.argv[2];
  if (cmd === 'encrypt') {
    encrypt();
  } else if (cmd === 'decrypt') {
    const content = decrypt();
    if (content) {
      console.warn('[WARNING] Sensitive data follows:');
      console.log(content);
    } else {
      console.error('No .env.encrypted found or decryption failed');
    }
  } else {
    console.log('Usage: node secure-env.js <encrypt|decrypt>');
  }
}

module.exports = { loadSecureEnv };
