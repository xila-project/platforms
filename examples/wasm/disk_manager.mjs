import { nextDiskName, isManagedDiskName } from './disk_names.mjs';
import {
  MEMORY_DISK,
  clearDiskRestart,
  prepareDiskRestart,
  readDiskSelection,
  readLastSavedDisk,
  readSeedVersion,
  rememberSavedDisk,
  takeDiskStartup,
  writeDiskSelection,
  writeSeedVersion,
} from './disk_preferences.mjs';

export { takeDiskStartup, writeSeedVersion };

const DISK_LIST_LOCK = 'xila-opfs-disk-list';

const sidebar = document.getElementById('sidebar');
const persistenceSwitch = document.getElementById('disk-persistence');
const savedDiskControls = document.getElementById('saved-disk-controls');
const memoryNotice = document.getElementById('memory-disk-notice');
const list = document.getElementById('disk-list');
const status = document.getElementById('disk-status');
const selectButton = document.getElementById('disk-select');
const createButton = document.getElementById('disk-create');
const removeButton = document.getElementById('disk-remove');
const rebuildButton = document.getElementById('disk-rebuild');

let availableDisks = [];
let storageAvailable = false;
let activeDisk = null;
let startupFailedDisk = null;
let recoverableDisk = null;
let currentSeedVersion = '';
let restartSelection = null;
let selectionChanges = 0;
let diskOperationPending = false;

const displayName = (name) => name === MEMORY_DISK ? 'Memory' : name;

function setStatus(message, isError = false) {
  status.textContent = message;
  status.dataset.error = String(isError);
}

async function diskFiles(root) {
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
  const persistent = isManagedDiskName(restartSelection ?? activeDisk ?? startupFailedDisk);
  const canSwitch = (activeDisk !== null || startupFailedDisk !== null)
    && restartSelection === null && !diskOperationPending;
  const canManageDisks = canSwitch && persistent && storageAvailable;
  persistenceSwitch.checked = persistent;
  persistenceSwitch.disabled = !canSwitch;
  savedDiskControls.hidden = !persistent;
  memoryNotice.hidden = persistent;
  selectButton.disabled = !canManageDisks || !name || name === activeDisk;
  createButton.disabled = !canManageDisks;
  removeButton.disabled = !canManageDisks || !name || name === activeDisk;
  rebuildButton.hidden = !name || !currentSeedVersion
    || (name !== recoverableDisk && !versionNotice(name));
  rebuildButton.disabled = !canManageDisks;
}

function render(selected) {
  list.replaceChildren();
  for (const name of availableDisks) {
    const option = document.createElement('option');
    option.value = name;
    const notice = versionNotice(name);
    option.textContent = [name, notice && `(${notice})`, name === activeDisk && '(open)']
      .filter(Boolean).join(' ');
    list.append(option);
  }
  list.value = availableDisks.includes(selected) ? selected : availableDisks[0] ?? '';
  updateControls();
}

async function findOrCreateDisk(preferredName) {
  return navigator.locks.request(DISK_LIST_LOCK, async () => {
    const root = await navigator.storage.getDirectory();
    const files = await diskFiles(root);
    if (files.includes(preferredName)) return preferredName;
    const name = nextDiskName(new Date(), files);
    await root.getFileHandle(name, { create: true });
    return name;
  });
}

async function refresh(selected) {
  const changesBeforeRefresh = selectionChanges;
  try {
    availableDisks = await diskFiles(await navigator.storage.getDirectory());
    storageAvailable = Boolean(navigator.locks);
  } catch {
    availableDisks = [];
    storageAvailable = false;
    if (startupFailedDisk === null && restartSelection === null) {
      setStatus('Saved disks are unavailable in this browser.', true);
    }
  }
  render(changesBeforeRefresh === selectionChanges ? selected ?? list.value : list.value);
}

function requestDiskSwitch(name, rebuild = false) {
  if (!prepareDiskRestart(name, rebuild) && rebuild) {
    setStatus('Browser storage could not remember the rebuild request.', true);
    return;
  }
  if (!writeDiskSelection(name) && (name !== MEMORY_DISK || readDiskSelection() !== MEMORY_DISK)) {
    clearDiskRestart();
    setStatus('Browser preferences are unavailable. Xila could not remember your disk choice.', true);
    return;
  }
  restartSelection = name;
  setStatus(`Restarting Xila using ${displayName(name)}…`);
  updateControls();
  if (startupFailedDisk !== null) window.location.reload();
  else window.dispatchEvent(new Event('xila-disk-switch'));
}

function confirmDiskSwitch(name) {
  if (window.confirm(`Restart Xila using ${displayName(name)}?`)) {
    requestDiskSwitch(name);
  }
}

persistenceSwitch.addEventListener('change', async () => {
  const enablePersistence = persistenceSwitch.checked;
  updateControls();
  if (!enablePersistence) {
    const saveNotice = isManagedDiskName(activeDisk) ? ' Xila will save your files before restarting.' : '';
    if (window.confirm(`Restart Xila in Memory?${saveNotice}`)) requestDiskSwitch(MEMORY_DISK);
    return;
  }

  if (!navigator.locks || !navigator.storage?.getDirectory) {
    setStatus('Saved disks are unavailable in this browser.', true);
    return;
  }
  if (!window.confirm('Restart Xila with saved files enabled? Changes in Memory will be discarded.')) return;
  diskOperationPending = true;
  updateControls();
  try {
    requestDiskSwitch(await findOrCreateDisk(readLastSavedDisk()));
  } catch (error) {
    setStatus(`Could not enable saved files: ${error.message}`, true);
  } finally {
    diskOperationPending = false;
    updateControls();
  }
});

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
  diskOperationPending = true;
  updateControls();
  try {
    const name = await findOrCreateDisk();
    await refresh(name);
    setStatus(`${name} created. Select it to restart Xila on that disk.`);
    confirmDiskSwitch(name);
  } catch (error) {
    setStatus(`Could not create a disk: ${error.message}`, true);
  } finally {
    diskOperationPending = false;
    updateControls();
  }
});

removeButton.addEventListener('click', async () => {
  const name = list.value;
  if (!window.confirm(`Permanently remove ${name} and all files stored on it?`)) return;
  diskOperationPending = true;
  updateControls();
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
  } finally {
    diskOperationPending = false;
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
  if (isManagedDiskName(name)) {
    rememberSavedDisk(name);
    if (!availableDisks.includes(name)) availableDisks.push(name);
  }
  render(name);
  if (persistenceSwitch.checked && sidebar.classList.contains('open')) refresh();
}

export function diskManagerStartupFailed(name, message, canRebuild) {
  startupFailedDisk = name;
  recoverableDisk = canRebuild ? name : null;
  setStatus(message, true);
  updateControls();
  if (isManagedDiskName(name)) {
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
  if (persistenceSwitch.checked && sidebar.classList.contains('open')) refresh();
});
