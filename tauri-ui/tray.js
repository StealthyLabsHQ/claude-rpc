const invoke = window.__TAURI__.core.invoke;

const menu = document.querySelector('#menu');
const versionEl = document.querySelector('#version');
const dndItem = document.querySelector('#dnd');
const startupItem = document.querySelector('#startup');
const startupLabel = document.querySelector('#startup-label');
const updateItem = document.querySelector('#update');
const updateLabel = document.querySelector('#update-label');
const updateBadge = document.querySelector('#update-badge');
const modeItems = {
  playing: document.querySelector('#mode-playing'),
  watching: document.querySelector('#mode-watching'),
  listening: document.querySelector('#mode-listening'),
  competing: document.querySelector('#mode-competing'),
};

let busy = false;

function applyTheme() {
  const saved = localStorage.getItem('claude-rpc-theme') || 'dark';
  const resolved =
    saved === 'system'
      ? window.matchMedia('(prefers-color-scheme: light)').matches
        ? 'light'
        : 'dark'
      : saved;
  document.body.dataset.theme = ['dark', 'light'].includes(resolved) ? resolved : 'dark';
}

function render(state) {
  if (!state) return;
  versionEl.textContent = `v${state.appVersion}`;
  dndItem.classList.toggle('on', state.dnd);
  startupItem.classList.toggle('on', state.startOnWindows);
  startupLabel.textContent = state.startupLabel;
  Object.entries(modeItems).forEach(([mode, item]) => {
    item.classList.toggle('checked', state.rpcMode === mode);
  });
  if (state.updateVersion) {
    updateItem.classList.add('update-ready');
    updateLabel.textContent = `Install Update v${state.updateVersion}`;
    updateBadge.hidden = false;
  } else {
    updateItem.classList.remove('update-ready');
    updateLabel.textContent = 'Check for Updates';
    updateBadge.hidden = true;
  }
}

async function refresh() {
  applyTheme();
  try {
    render(await invoke('tray_state'));
  } catch {
    /* daemon state unavailable; keep last render */
  }
  menu.classList.remove('pop');
  void menu.offsetWidth;
  menu.classList.add('pop');
}

async function act(action) {
  if (busy) return;
  busy = true;
  if (action === 'update' && !updateItem.classList.contains('update-ready')) {
    updateLabel.textContent = 'Checking…';
  }
  try {
    render(await invoke('tray_action', { action }));
  } catch {
    updateLabel.textContent = 'Check for Updates';
  } finally {
    busy = false;
  }
}

document.querySelectorAll('.item').forEach((item) => {
  item.addEventListener('click', () => act(item.dataset.action));
});

document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') act('close');
});

window.addEventListener('focus', refresh);
refresh();
