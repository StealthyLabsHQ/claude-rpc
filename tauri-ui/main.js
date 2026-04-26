const invoke = window.__TAURI__.core.invoke;

const presets = {
  claude: ['Claude', 'https://claude.ai'],
  desktop: ['Claude Desktop', 'https://claude.ai/download'],
  repo: ['GitHub Repo', 'https://github.com/StealthyLabsHQ/claude-rpc'],
};

const fields = {
  mode: document.querySelector('#rpc-mode'),
  logoMode: document.querySelector('#logo-mode'),
  dndToggle: document.querySelector('#dnd-toggle'),
  providerToggle: document.querySelector('#provider-toggle'),
  effortToggle: document.querySelector('#effort-toggle'),
  limitsToggle: document.querySelector('#limits-toggle'),
  limit5hToggle: document.querySelector('#limit-5h-toggle'),
  limitAllToggle: document.querySelector('#limit-all-toggle'),
  limitSonnetToggle: document.querySelector('#limit-sonnet-toggle'),
  limitDesignToggle: document.querySelector('#limit-design-toggle'),
  refreshLimits: document.querySelector('#refresh-limits'),
  debugToggle: document.querySelector('#debug-toggle'),
  labels: [document.querySelector('#label0'), document.querySelector('#label1')],
  urls: [document.querySelector('#url0'), document.querySelector('#url1')],
  status: document.querySelector('#status'),
  message: document.querySelector('#message'),
  previewActivity: document.querySelector('#preview-activity'),
  previewDetails: document.querySelector('#preview-details'),
  previewState: document.querySelector('#preview-state'),
  previewButtons: document.querySelector('#preview-buttons'),
  themeButtons: [...document.querySelectorAll('[data-theme-option]')],
  sectionToggles: [...document.querySelectorAll('.section-toggle')],
};

let loading = true;
let saveTimer = null;
let currentConfig = {};
let currentStatus = {
  claudeLine: 'Claude: Off',
  modelLine: 'Auto-detect',
  providerLine: 'Provider: Unknown',
  discordLine: 'Discord: RPC disabled',
};

function readForm() {
  const buttons = [];
  for (let i = 0; i < 2; i += 1) {
    const label = fields.labels[i].value.trim();
    const url = fields.urls[i].value.trim();
    if (label && url) buttons.push({ label, url });
  }
  return {
    ...currentConfig,
    logoMode: fields.logoMode.value,
    dnd: fields.dndToggle.dataset.enabled === 'true',
    showLimits: fields.limitsToggle.dataset.enabled === 'true',
    showLimit5h: fields.limit5hToggle.dataset.enabled === 'true',
    showLimitAll: fields.limitAllToggle.dataset.enabled === 'true',
    showLimitSonnet: fields.limitSonnetToggle.dataset.enabled === 'true',
    showLimitDesign: fields.limitDesignToggle.dataset.enabled === 'true',
    showProvider: fields.providerToggle.dataset.enabled === 'true',
    showEffort: fields.effortToggle.dataset.enabled === 'true',
    verbose: fields.debugToggle.dataset.enabled === 'true',
    rpcMode: fields.mode.value,
    buttons,
  };
}

function writeForm(config) {
  currentConfig = config || {};
  fields.mode.value = currentConfig.rpcMode || 'playing';
  fields.logoMode.value = currentConfig.logoMode || 'url';
  syncToggle(fields.dndToggle, !!currentConfig.dnd, 'DND');
  syncToggle(fields.providerToggle, currentConfig.showProvider !== false, 'Provider', '', '');
  syncToggle(fields.effortToggle, currentConfig.showEffort !== false, 'Effort', '', '');
  syncToggle(fields.limitsToggle, currentConfig.showLimits !== false, '', 'Shown', 'Hidden');
  syncToggle(fields.limit5hToggle, currentConfig.showLimit5h !== false, '5h', '', '');
  syncToggle(fields.limitAllToggle, currentConfig.showLimitAll !== false, 'All', '', '');
  syncToggle(fields.limitSonnetToggle, currentConfig.showLimitSonnet !== false, 'Sonnet only', '', '');
  syncToggle(fields.limitDesignToggle, currentConfig.showLimitDesign !== false, 'Design', '', '');
  syncToggle(fields.debugToggle, !!currentConfig.verbose, 'Debug');
  syncLimitControls();
  for (let i = 0; i < 2; i += 1) {
    fields.labels[i].value = currentConfig.buttons?.[i]?.label || '';
    fields.urls[i].value = currentConfig.buttons?.[i]?.url || '';
  }
  syncMode();
  updatePreview();
}

function syncToggle(button, enabled, label, onLabel = 'on', offLabel = 'off') {
  button.dataset.enabled = String(enabled);
  button.textContent = [label, enabled ? onLabel : offLabel].filter(Boolean).join(' ');
  button.classList.toggle('active', enabled);
  button.setAttribute('aria-pressed', String(enabled));
}

function syncMode() {
  const enabled = fields.mode.value === 'watching';
  for (const input of [...fields.labels, ...fields.urls]) input.disabled = !enabled;
  document.querySelectorAll('.presets button, #clear').forEach((button) => {
    button.disabled = !enabled;
  });
}

function syncLimitControls() {
  const enabled = fields.limitsToggle.dataset.enabled === 'true';
  [fields.limit5hToggle, fields.limitAllToggle, fields.limitSonnetToggle, fields.limitDesignToggle].forEach(
    (button) => {
      button.disabled = !enabled;
    },
  );
}

async function refreshLimits() {
  try {
    const config = readForm();
    await invoke('save_config', { config });
    currentConfig = config;
    await invoke('refresh_limits');
    fields.message.textContent = 'open Usage page';
    setTimeout(refreshStatus, 1500);
  } catch (error) {
    fields.message.textContent = String(error);
  }
}

async function load() {
  try {
    applyTheme(localStorage.getItem('claude-rpc-theme') || 'dark');
    await invoke('start_daemon');
    writeForm(await invoke('load_config'));
    await refreshStatus();
    loading = false;
  } catch (error) {
    fields.message.textContent = String(error);
    loading = false;
  }
}

async function refreshStatus() {
  try {
    currentStatus = await invoke('load_status');
  } catch {
    currentStatus = {
      claudeLine: 'Claude: Off',
      modelLine: 'Auto-detect',
      providerLine: 'Provider: Unknown',
      discordLine: 'Discord: RPC disabled',
    };
  }
  fields.status.textContent = formatStatus(currentStatus);
  updatePreview();
}

function formatStatus(status) {
  const config = readForm();
  const parts = [
    status.claudeLine,
    formatModelForRpc(status.modelLine, config.showEffort !== false),
    config.showProvider !== false ? status.providerLine : '',
    status.discordLine,
  ]
    .concat([config.showLimits !== false ? status.limitsLine : '', status.debugLine])
    .filter(Boolean)
    .map((part) => part.replace(/^Model:\s*/i, ''));
  if (status.daemonError) parts.push(status.daemonError);
  return parts.join(' | ');
}

function updatePreview() {
  const config = readForm();
  const mode = config.rpcMode || 'playing';
  fields.previewActivity.textContent =
    {
      watching: 'Watching Claude',
      listening: 'Listening Claude',
      competing: 'Competing Claude',
      playing: 'Playing Claude',
    }[mode] || 'Playing Claude';
  fields.previewDetails.textContent = previewDetails(currentStatus.claudeLine);
  fields.previewState.textContent = [
    formatModelForRpc(currentStatus.modelLine, config.showEffort !== false),
    config.showProvider !== false ? currentStatus.providerLine.replace(/^Provider:\s*/i, '') : '',
  ]
    .filter(Boolean)
    .join(' | ');
  fields.previewState.title = currentStatus.limitsLine || '';
  fields.previewState.dataset.limits = currentStatus.limitsLine || '';
  renderPreviewButtons(mode, config.buttons || []);
}

function formatModelForRpc(model, showEffort) {
  if (showEffort) return model;
  const parts = model.split(' | ').filter((part) => {
    const label = part.trim().toLowerCase();
    return !['low', 'medium', 'high', 'extra high', 'xhigh', 'max'].includes(label);
  });
  return parts.join(' | ') || model;
}

function previewDetails(line) {
  if (line.includes('Desktop')) return line.replace(/^Claude:\s*/i, 'Claude ');
  if (line.includes('CLI') || line.includes('Code')) return 'Claude Code';
  return 'No Claude activity';
}

function renderPreviewButtons(mode, buttons) {
  fields.previewButtons.replaceChildren();
  fields.previewButtons.hidden = mode !== 'watching' || buttons.length === 0;
  if (fields.previewButtons.hidden) return;
  for (const button of buttons.slice(0, 2)) {
    const item = document.createElement('span');
    item.textContent = button.label;
    fields.previewButtons.appendChild(item);
  }
}

async function save(kind = 'manual') {
  try {
    const config = readForm();
    await invoke('save_config', { config });
    currentConfig = config;
    fields.message.textContent = kind === 'auto' ? 'saved' : 'applied';
    fields.status.textContent = formatStatus(currentStatus);
    updatePreview();
  } catch (error) {
    fields.message.textContent = String(error);
  }
}

function scheduleSave() {
  if (loading) return;
  clearTimeout(saveTimer);
  fields.message.textContent = 'saving...';
  fields.status.textContent = formatStatus(currentStatus);
  updatePreview();
  saveTimer = setTimeout(() => save('auto'), 300);
}

document.querySelector('#apply').addEventListener('click', () => save());
document.querySelector('#close').addEventListener('click', () => invoke('close_settings'));
fields.refreshLimits.addEventListener('click', refreshLimits);
document.querySelector('#clear').addEventListener('click', () => {
  for (const input of [...fields.labels, ...fields.urls]) input.value = '';
  scheduleSave();
});
fields.mode.addEventListener('change', () => {
  syncMode();
  scheduleSave();
});
fields.logoMode.addEventListener('change', scheduleSave);
fields.dndToggle.addEventListener('click', () => {
  syncToggle(fields.dndToggle, fields.dndToggle.dataset.enabled !== 'true', 'DND');
  scheduleSave();
});
[fields.providerToggle, fields.effortToggle].forEach((button) => {
  button.addEventListener('click', () => {
    syncToggle(button, button.dataset.enabled !== 'true', button.textContent, '', '');
    scheduleSave();
  });
});
fields.limitsToggle.addEventListener('click', () => {
  syncToggle(
    fields.limitsToggle,
    fields.limitsToggle.dataset.enabled !== 'true',
    '',
    'Shown',
    'Hidden',
  );
  syncLimitControls();
  scheduleSave();
});
[
  [fields.limit5hToggle, '5h'],
  [fields.limitAllToggle, 'All'],
  [fields.limitSonnetToggle, 'Sonnet only'],
  [fields.limitDesignToggle, 'Design'],
].forEach(([button, label]) => {
  button.addEventListener('click', () => {
    syncToggle(button, button.dataset.enabled !== 'true', label, '', '');
    scheduleSave();
  });
});
fields.debugToggle.addEventListener('click', () => {
  syncToggle(fields.debugToggle, fields.debugToggle.dataset.enabled !== 'true', 'Debug');
  scheduleSave();
});
for (const input of [...fields.labels, ...fields.urls]) input.addEventListener('input', scheduleSave);
fields.themeButtons.forEach((button) => {
  button.addEventListener('click', () => applyTheme(button.dataset.themeOption));
});

document.querySelectorAll('[data-preset]').forEach((button) => {
  button.addEventListener('click', () => {
    const [label, url] = presets[button.dataset.preset];
    const slot = fields.labels[0].value.trim() ? 1 : 0;
    fields.labels[slot].value = label;
    fields.urls[slot].value = url;
    scheduleSave();
  });
});

function applyTheme(theme) {
  const safeTheme = ['dark', 'system', 'light'].includes(theme) ? theme : 'dark';
  const resolved =
    safeTheme === 'system'
      ? window.matchMedia('(prefers-color-scheme: light)').matches
        ? 'light'
        : 'dark'
      : safeTheme;
  document.body.dataset.theme = resolved;
  fields.themeButtons.forEach((button) => {
    const active = button.dataset.themeOption === safeTheme;
    button.classList.toggle('active', active);
    button.setAttribute('aria-pressed', String(active));
  });
  localStorage.setItem('claude-rpc-theme', safeTheme);
}

function initSections() {
  fields.sectionToggles.forEach((button) => {
    const panel = button.closest('.panel');
    const key = `claude-rpc-section-${panel.dataset.section}`;
    const sync = (expanded) => {
      panel.classList.toggle('collapsed', !expanded);
      button.setAttribute('aria-expanded', String(expanded));
    };

    sync(localStorage.getItem(key) !== 'collapsed');
    button.addEventListener('click', () => {
      const expanded = button.getAttribute('aria-expanded') !== 'true';
      sync(expanded);
      localStorage.setItem(key, expanded ? 'expanded' : 'collapsed');
    });
  });
}

initSections();
window.addEventListener('DOMContentLoaded', load);
setInterval(refreshStatus, 1000);
