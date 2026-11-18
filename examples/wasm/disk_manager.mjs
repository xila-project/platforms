import { nextDiskName, isManagedDiskName } from './disk_names.mjs';
import {
  MEMORY_DISK,
  clearDiskRestart,
  prepareDiskRestart,
  readDiskSelection,
  readSeedVersion,
  takeDiskStartup,
  writeDiskSelection,
  writeSeedVersion,
} from './disk_preferences.mjs';

export { takeDiskStartup, writeSeedVersion };

const DISK_LIST_LOCK = 'xila-opfs-disk-list';
const MEMORY_LABEL = 'Memory (resets on reload)';

const sidebar = document.getElementById('sidebar');
const list = document.getElementById('disk-list');
const status = document.getElementById('disk-status');
const selectButton = document.getElementById('disk-select');
const createButton = document.getElementById('disk-create');
const removeButton = document.getElementById('disk-remove');
const rebuildButton = document.getElementById('disk-rebuild');

let availableDisks = [];
let storageAvailable = false;
let activeDisk = null;
let startupFailed = false;
let recoverableDisk = null;
let currentSeedVersion = '';
let restartSelection = null;
let selectionChanges = 0;

const displayName = (name) => name === MEMORY_DISK ? 'Memory' : name;

function setStatus(message, isError = false) {
  status.textContent = message;
  status.dataset.error = String(isError);
}

async function diskFiles() {
  const root = await navigator.storage.getDirectory();
  const files = [];
  for await (const [name, handle] of root.entries()) {
    if (handle.kind === 'file' && isManagedDiskName(name)) files.push(name);
  }
  return files.sort((left, right) => left.localeCompare(right));
}

function versionNotice(name) {
  const version = readSeedVersion(name);
  if (!version) return 'seed version unknown';
  if (currentSeedVersion && version !== currentSeedVersion) return 'older seed version';
  return '';
}

function updateControls() {
  const name = list.value;
  const canSwitch = (activeDisk !== null || startupFailed) && restartSelection === null;
  const saved = isManagedDiskName(name);
  selectButton.disabled = !canSwitch || !name || name === activeDisk;
  createButton.disabled = !canSwitch || !storageAvailable;
  removeButton.disabled = !canSwitch || !storageAvailable || !saved || name === activeDisk;
  rebuildButton.hidden = !saved || !currentSeedVersion
    || (name !== recoverableDisk && !versionNotice(name));
  rebuildButton.disabled = !canSwitch || !storageAvailable;
}

function render(selected) {
  list.replaceChildren();
  for (const name of [MEMORY_DISK, ...availableDisks]) {
    const option = document.createElement('option');
    option.value = name;
    const notice = name === MEMORY_DISK ? '' : versionNotice(name);
    const label = name === MEMORY_DISK ? MEMORY_LABEL : name;
    option.textContent = [label, notice && `(${notice})`, name === activeDisk && name !== MEMORY_DISK && '(open)']
      .filter(Boolean).join(' ');
    list.append(option);
  }
  list.value = availableDisks.includes(selected) ? selected : MEMORY_DISK;
  updateControls();
}

async function refresh(selected) {
  const changesBeforeRefresh = selectionChanges;
  try {
    availableDisks = await diskFiles();
    storageAvailable = Boolean(navigator.locks);
  } catch {
    availableDisks = [];
    storageAvailable = false;
    if (!startupFailed) setStatus('Saved disks are unavailable in this browser.', true);
  }
  render(changesBeforeRefresh === selectionChanges ? selected ?? list.value : list.value);
}

function requestDiskSwitch(name, rebuild = false) {
  if (!prepareDiskRestart(name, rebuild) && rebuild) {
    setStatus('Browser storage could not remember the rebuild request.', true);
    return;
  }
  if (!writeDiskSelection(name)) {
    clearDiskRestart();
    setStatus('Browser preferences are unavailable. Xila will use Memory on reload.', true);
    if (name === MEMORY_DISK && startupFailed) window.location.reload();
    return;
  }
  restartSelection = name;
  setStatus(`Restarting Xila using ${displayName(name)}…`);
  updateControls();
  if (startupFailed) window.location.reload();
  else window.dispatchEvent(new Event('xila-disk-switch'));
}

function confirmDiskSwitch(name) {
  const discarded = activeDisk === MEMORY_DISK ? ' Changes on the memory disk will be discarded.' : '';
  if (window.confirm(`Restart Xila using ${displayName(name)}?${discarded}`)) {
    requestDiskSwitch(name);
  }
}

list.addEventListener('change', () => {
  selectionChanges++;
  updateControls();
});
selectButton.addEventListener('click', () => confirmDiskSwitch(list.value));
rebuildButton.addEventListener('click', () => {
  const name = list.value;
  if (window.confirm(`Rebuild ${name} from the current version? This permanently discards all files and settings on that disk.`)) {
    requestDiskSwitch(name, true);
  }
});

createButton.addEventListener('click', async () => {
  createButton.disabled = true;
  try {
    const name = await navigator.locks.request(DISK_LIST_LOCK, async () => {
      const files = new Set(await diskFiles());
      const candidate = nextDiskName(new Date(), files);
      const root = await navigator.storage.getDirectory();
      await root.getFileHandle(candidate, { create: true });
      return candidate;
    });
    await refresh(name);
    setStatus(`${name} created. Select it to restart Xila on that disk.`);
    if (restartSelection === null) confirmDiskSwitch(name);
  } catch (error) {
    setStatus(`Could not create a disk: ${error.message}`, true);
    updateControls();
  }
});

removeButton.addEventListener('click', async () => {
  const name = list.value;
  if (!window.confirm(`Permanently remove ${name} and all files stored on it?`)) return;
  removeButton.disabled = true;
  try {
    await navigator.locks.request(`xila-opfs:${name}`, { ifAvailable: true }, async (lock) => {
      if (!lock) throw new Error('This disk is open in another Xila tab. Close that tab first.');
      await (await navigator.storage.getDirectory()).removeEntry(name);
      writeSeedVersion(name, '');
      if (readDiskSelection() === name) writeDiskSelection(MEMORY_DISK);
    });
    await refresh();
    setStatus(`${name} removed.`);
  } catch (error) {
    setStatus(`Could not remove ${name}: ${error.message}`, true);
    updateControls();
  }
});

export function setSeedVersion(version) {
  currentSeedVersion = version;
  render(list.value);
}

export function diskManagerReady(name) {
  activeDisk = name;
  setStatus(`Using ${displayName(name)}.`);
  if (isManagedDiskName(name) && !availableDisks.includes(name)) availableDisks.push(name);
  render(name);
}

export function diskManagerStartupFailed(name, message, canRebuild) {
  startupFailed = true;
  recoverableDisk = canRebuild ? name : null;
  setStatus(message, true);
  if (name === MEMORY_DISK) {
    updateControls();
  } else {
    if (!sidebar.classList.contains('open')) window.toggleSidebar();
    refresh(name);
  }
}

export function diskManagerError(message) {
  if (restartSelection !== null) {
    clearDiskRestart();
    if (readDiskSelection() === restartSelection) writeDiskSelection(activeDisk);
    restartSelection = null;
  }
  setStatus(message, true);
  updateControls();
}

document.getElementById('sidebar-toggle').addEventListener('click', () => {
  if (sidebar.classList.contains('open')) refresh();
});
