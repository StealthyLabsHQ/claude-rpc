import { describe, it, expect } from 'vitest';
import { createRequire } from 'module';

const require = createRequire(import.meta.url);
const { formatModelName, compareVersions, sanitizeString, loadConfig, DEFAULT_CONFIG } = require('../index');

// --- formatModelName ---

describe('formatModelName', () => {
  it('returns null for empty input', () => {
    expect(formatModelName(null)).toBeNull();
    expect(formatModelName('')).toBeNull();
    expect(formatModelName(undefined)).toBeNull();
  });

  it('formats Opus models', () => {
    expect(formatModelName('claude-opus-4-6')).toBe('Claude Opus 4.6 (1M)');
    expect(formatModelName('claude-opus-4-5')).toBe('Claude Opus 4.5 (1M)');
  });

  it('formats Sonnet models', () => {
    expect(formatModelName('claude-sonnet-4-6')).toBe('Claude Sonnet 4.6');
    expect(formatModelName('claude-sonnet-4-5')).toBe('Claude Sonnet 4.5');
  });

  it('formats Haiku models', () => {
    expect(formatModelName('claude-haiku-4-5')).toBe('Claude Haiku 4.5');
  });

  it('handles Opus Plan mode', () => {
    expect(formatModelName('opusplan')).toBe('Opus Plan / Sonnet 4.6');
  });

  it('detects 1M context for eligible models', () => {
    expect(formatModelName('claude-opus-4-6')).toContain('(1M)');
    expect(formatModelName('claude-opus-4-5')).toContain('(1M)');
    expect(formatModelName('claude-sonnet-4-6')).not.toContain('(1M)');
  });

  it('handles explicit context markers', () => {
    expect(formatModelName('claude-sonnet-4-6[1m]')).toContain('(1M)');
  });

  it('falls back to sanitized input for unknown models', () => {
    expect(formatModelName('custom-model')).toBe('custom-model');
  });

  it('defaults to latest version when version is missing', () => {
    expect(formatModelName('claude-opus')).toContain('4.6');
    expect(formatModelName('claude-sonnet')).toContain('4.6');
    expect(formatModelName('claude-haiku')).toContain('4.5');
  });
});

// --- compareVersions ---

describe('compareVersions', () => {
  it('returns 0 for equal versions', () => {
    expect(compareVersions('1.0.0', '1.0.0')).toBe(0);
    expect(compareVersions('2.1.0', '2.1.0')).toBe(0);
  });

  it('returns 1 when a > b', () => {
    expect(compareVersions('2.0.0', '1.0.0')).toBe(1);
    expect(compareVersions('1.1.0', '1.0.0')).toBe(1);
    expect(compareVersions('1.0.1', '1.0.0')).toBe(1);
  });

  it('returns -1 when a < b', () => {
    expect(compareVersions('1.0.0', '2.0.0')).toBe(-1);
    expect(compareVersions('1.0.0', '1.1.0')).toBe(-1);
  });

  it('handles different length versions', () => {
    expect(compareVersions('1.0.0', '1.0')).toBe(0);
    expect(compareVersions('1.0.1', '1.0')).toBe(1);
  });
});

// --- sanitizeString ---

describe('sanitizeString', () => {
  it('returns empty for non-strings', () => {
    expect(sanitizeString(null)).toBe('');
    expect(sanitizeString(123)).toBe('');
    expect(sanitizeString(undefined)).toBe('');
  });

  it('preserves normal text', () => {
    expect(sanitizeString('Hello World')).toBe('Hello World');
    expect(sanitizeString('my-project.v2')).toBe('my-project.v2');
  });

  it('strips dangerous characters', () => {
    expect(sanitizeString('test<script>')).not.toContain('<');
    expect(sanitizeString('test<script>')).not.toContain('>');
  });

  it('truncates long strings', () => {
    const long = 'a'.repeat(200);
    expect(sanitizeString(long).length).toBeLessThanOrEqual(128);
  });

  it('accepts custom max length', () => {
    expect(sanitizeString('abcdefgh', 5).length).toBeLessThanOrEqual(5);
  });
});

// --- Config ---

describe('config', () => {
  it('DEFAULT_CONFIG has expected keys', () => {
    expect(DEFAULT_CONFIG).toHaveProperty('idleTimeoutMinutes', 15);
    expect(DEFAULT_CONFIG).toHaveProperty('logoMode', 'url');
    expect(DEFAULT_CONFIG).toHaveProperty('dnd', false);
    expect(DEFAULT_CONFIG).toHaveProperty('verbose', false);
    expect(DEFAULT_CONFIG).toHaveProperty('webhookUrl', null);
  });

  it('loadConfig returns defaults when no config file exists', () => {
    const cfg = loadConfig();
    expect(cfg.idleTimeoutMinutes).toBe(15);
    expect(cfg.dnd).toBe(false);
  });
});
