import { describe, it, expect } from 'vitest';
import { createRequire } from 'module';

const require = createRequire(import.meta.url);
const {
  formatModelName,
  inferDesktopUiState,
  formatDesktopModeLabel,
  formatDesktopModelLabel,
  buildPresenceChangeKey,
  compareVersions,
  sanitizeString,
  loadConfig,
  DEFAULT_CONFIG,
  buildTrayStatus,
  getCodePresenceLabels,
  TRAY_APP_NAME,
} = require('../index');

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
    expect(DEFAULT_CONFIG).toHaveProperty('logoMode', 'url');
    expect(DEFAULT_CONFIG).toHaveProperty('dnd', false);
    expect(DEFAULT_CONFIG).toHaveProperty('verbose', false);
    expect(DEFAULT_CONFIG).toHaveProperty('webhookUrl', null);
  });

  it('loadConfig returns defaults when no config file exists', () => {
    const cfg = loadConfig();
    expect(cfg.dnd).toBe(false);
    expect(cfg.logoMode).toBe('url');
  });
});

// --- Tray status ---

describe('buildTrayStatus', () => {
  it('uses the renamed app label for the tray', () => {
    expect(TRAY_APP_NAME).toBe('Claude Rich Presence');
  });

  it('shows claude/model/provider/discord for desktop Code sessions', () => {
    const status = buildTrayStatus({
      clientType: 'desktop',
      clientMode: 'Code',
      model: 'Claude Sonnet 4.6',
      provider: 'Anthropic API',
      discordConnected: true,
    });

    expect(status.summary).toBe('Claude Rich Presence');
    expect(status.claudeLine).toBe('Claude: Desktop (Code)');
    expect(status.modelLine).toBe('Claude Sonnet 4.6');
    expect(status.providerLine).toBe('Provider: Anthropic API');
    expect(status.discordLine).toBe('Discord: Connected');
  });

  it('shows CLI label and RPC disabled when code client and Discord not connected', () => {
    const status = buildTrayStatus({
      clientType: 'code',
      model: 'Claude Opus 4.6',
      discordConnected: false,
      provider: 'Claude Account',
    });

    expect(status.summary).toBe('Claude Rich Presence');
    expect(status.claudeLine).toBe('Claude: CLI (Code)');
    expect(status.modelLine).toBe('Claude Opus 4.6');
    expect(status.providerLine).toBe('Provider: Claude Account');
    expect(status.discordLine).toBe('Discord: RPC disabled');
  });

  it('falls back to Off/Auto-detect/Unknown when values are missing', () => {
    const status = buildTrayStatus({});

    expect(status.summary).toBe('Claude Rich Presence');
    expect(status.claudeLine).toBe('Claude: Off');
    expect(status.modelLine).toBe('Auto-detect');
    expect(status.providerLine).toBe('Provider: Unknown');
    expect(status.discordLine).toBe('Discord: RPC disabled');
  });

  it('includes submode in Desktop label for Cowork - Dispatch', () => {
    const status = buildTrayStatus({
      clientType: 'desktop',
      clientMode: 'Cowork',
      clientSubmode: 'Dispatch',
    });

    expect(status.claudeLine).toBe('Claude: Desktop (Cowork - Dispatch)');
  });
});

// --- Discord RPC labels ---

describe('getCodePresenceLabels', () => {
  it('does not include the multi-instance count in RPC labels', () => {
    const labels = getCodePresenceLabels(2);

    expect(labels.details).toBe('Claude Code');
    expect(labels.smallImageText).toBe('Claude Code CLI');
    expect(labels.details).not.toContain('[2]');
    expect(labels.smallImageText).not.toContain('[2]');
  });
});

// --- Claude Desktop labels ---

describe('formatDesktopModeLabel', () => {
  it('includes the Cowork submode when Dispatch is active', () => {
    expect(formatDesktopModeLabel('Cowork', 'Dispatch')).toBe('Cowork - Dispatch');
  });

  it('falls back to the base mode when no submode is present', () => {
    expect(formatDesktopModeLabel('Chat')).toBe('Chat');
  });
});

describe('formatDesktopModelLabel', () => {
  it('adds Adaptive when adaptive thinking is enabled', () => {
    expect(formatDesktopModelLabel('Claude Opus 4.7', { adaptive: true })).toBe('Claude Opus 4.7 Adaptive');
  });

  it('does not duplicate Adaptive when already present', () => {
    expect(formatDesktopModelLabel('Claude Opus 4.7 Adaptive', { adaptive: true })).toBe('Claude Opus 4.7 Adaptive');
  });
});

describe('inferDesktopUiState', () => {
  it('detects Dispatch inside Cowork views', () => {
    const state = inferDesktopUiState([
      'New task',
      'Computer use',
      'Code permissions',
      'Outputs',
      'Dispatch background conversation',
    ]);

    expect(state.mode).toBe('Cowork');
    expect(state.submode).toBe('Dispatch');
  });

  it('detects Code from the desktop dashboard controls', () => {
    const state = inferDesktopUiState([
      'New session',
      'Routines',
      "What's up next, Stealthy?",
      'Overview',
      'Favorite model',
    ]);

    expect(state.mode).toBe('Code');
    expect(state.submode).toBeNull();
  });

  it('detects Chat and Adaptive from chat composer controls', () => {
    const state = inferDesktopUiState([
      'New chat',
      'Artifacts',
      'Learn',
      'From Gmail',
      'Opus 4.7 Adaptive',
    ]);

    expect(state.mode).toBe('Chat');
    expect(state.model).toBe('Opus 4.7');
    expect(state.adaptive).toBe(true);
  });
});

describe('buildPresenceChangeKey', () => {
  it('changes when the desktop submode changes', () => {
    const cowork = buildPresenceChangeKey({
      details: 'Claude Desktop (Cowork)',
      state: 'Opus 4.7 | Anthropic API',
      assets: { smallText: 'Claude Desktop - Cowork' },
    });
    const dispatch = buildPresenceChangeKey({
      details: 'Claude Desktop (Cowork - Dispatch)',
      state: 'Opus 4.7 | Anthropic API',
      assets: { smallText: 'Claude Desktop - Cowork - Dispatch' },
    });

    expect(cowork).not.toBe(dispatch);
  });
});
