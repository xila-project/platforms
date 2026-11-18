import { isManagedDiskName } from './disk_names.mjs';

export const MEMORY_DISK = 'memory';

// The previous key selected a saved disk by default. Start with a fresh choice.
const SELECTED_DISK_KEY = 'xila.diskSelection';
const restartKey = () => `xila.diskRestart.${globalThis.location?.pathname ?? ''}`;
const seedVersionKey = (name) => `xila.seedVersion.${name}`;

function storedDiskSelection() {
  const name = localStorage.getItem(SELECTED_DISK_KEY);
  return isManagedDiskName(name) ? name : MEMORY_DISK;
}

export function readDiskSelection() {
  try {
    return storedDiskSelection();
  } catch {
    return MEMORY_DISK;
  }
}

export function writeDiskSelection(name) {
  try {
    localStorage.setItem(SELECTED_DISK_KEY, name);
    return true;
  } catch {
    return false;
  }
}

export function readSeedVersion(name) {
  try {
    return localStorage.getItem(seedVersionKey(name));
  } catch {
    return null;
  }
}

export function writeSeedVersion(name, version) {
  try {
    if (version) localStorage.setItem(seedVersionKey(name), version);
    else localStorage.removeItem(seedVersionKey(name));
  } catch {
    // A disk without metadata can still be opened.
  }
}

// A restart follows the choice in this tab even if another tab updates
// the shared preference before the page reloads.
export function prepareDiskRestart(name, rebuild) {
  try {
    sessionStorage.setItem(restartKey(), JSON.stringify({ name, rebuild }));
    return true;
  } catch {
    return false;
  }
}

export function clearDiskRestart() {
  try {
    sessionStorage.removeItem(restartKey());
  } catch {
    // Session storage can be disabled.
  }
}

export function takeDiskStartup() {
  let requested;
  try {
    const saved = sessionStorage.getItem(restartKey());
    sessionStorage.removeItem(restartKey());
    requested = JSON.parse(saved);
  } catch {
    // A normal startup only needs the shared preference.
  }

  let name;
  try {
    name = storedDiskSelection();
  } catch {
    return { name: MEMORY_DISK, rebuild: false };
  }

  if (requested?.name === MEMORY_DISK) return { name: MEMORY_DISK, rebuild: false };
  if (isManagedDiskName(requested?.name)) {
    return { name: requested.name, rebuild: requested.rebuild === true };
  }
  return { name, rebuild: false };
}
