import assert from 'node:assert/strict';
import test from 'node:test';

import { diskNameForDate, isManagedDiskName, nextDiskName } from '../disk_names.mjs';

test('new disk names use the local calendar date and suffix collisions', () => {
  const date = new Date(2026, 9, 16, 23, 30);
  assert.equal(diskNameForDate(date, 1), 'xila_2026-10-16.img');
  assert.equal(diskNameForDate(date, 2), 'xila_2026-10-16-2.img');
  assert.equal(
    nextDiskName(date, ['xila_2026-10-16.img', 'xila_2026-10-16-2.img']),
    'xila_2026-10-16-3.img',
  );
});

test('only Xila disk images are managed by the panel', () => {
  assert.equal(isManagedDiskName('xila_drive.img'), true);
  assert.equal(isManagedDiskName('xila_2026-10-16-2.img'), true);
  assert.equal(isManagedDiskName('unrelated.img'), false);
  assert.equal(isManagedDiskName('xila_2026-10-16.txt'), false);
});

test('existing saved selections start in memory while explicit choices survive reload', async () => {
  const { readDiskSelection, writeDiskSelection } = await import('../disk_preferences.mjs');
  const storage = new Map([['xila.selectedDisk', 'xila_drive.img']]);
  globalThis.localStorage = {
    getItem: (key) => storage.get(key),
    setItem: (key, value) => storage.set(key, value),
  };
  try {
    assert.equal(readDiskSelection(), 'memory');
    assert.equal(writeDiskSelection('xila_drive.img'), true);
    assert.equal(readDiskSelection(), 'xila_drive.img');
    assert.equal(writeDiskSelection('memory'), true);
    assert.equal(readDiskSelection(), 'memory');
  } finally {
    delete globalThis.localStorage;
  }
});

test('unavailable browser preferences fall back to memory', async () => {
  const { readDiskSelection, writeDiskSelection } = await import('../disk_preferences.mjs');
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    get() { throw new Error('Storage disabled'); },
  });
  try {
    assert.equal(readDiskSelection(), 'memory');
    assert.equal(writeDiskSelection('xila_drive.img'), false);
  } finally {
    delete globalThis.localStorage;
  }
});

test('a tab consumes its own confirmed rebuild once even if another tab selects a different disk', async () => {
  const { prepareDiskRestart, takeDiskStartup, writeDiskSelection } = await import('../disk_preferences.mjs');
  const makeStorage = () => {
    const values = new Map();
    return {
      getItem: (key) => values.get(key) ?? null,
      setItem: (key, value) => values.set(key, value),
      removeItem: (key) => values.delete(key),
    };
  };
  globalThis.localStorage = makeStorage();
  globalThis.sessionStorage = makeStorage();
  try {
    assert.equal(prepareDiskRestart('xila_drive.img', true), true);
    writeDiskSelection('xila_2026-10-16.img');
    assert.deepEqual(takeDiskStartup(), { name: 'xila_drive.img', rebuild: true });
    assert.deepEqual(takeDiskStartup(), { name: 'xila_2026-10-16.img', rebuild: false });
  } finally {
    delete globalThis.localStorage;
    delete globalThis.sessionStorage;
  }
});

test('turning saved files back on reopens the last saved disk that started', async () => {
  const { readLastSavedDisk, rememberSavedDisk } = await import('../disk_preferences.mjs');
  const storage = new Map();
  globalThis.localStorage = {
    getItem: (key) => storage.get(key) ?? null,
    setItem: (key, value) => storage.set(key, value),
  };
  try {
    assert.equal(readLastSavedDisk(), null);
    rememberSavedDisk('xila_2026-10-16.img');
    assert.equal(readLastSavedDisk(), 'xila_2026-10-16.img');
  } finally {
    delete globalThis.localStorage;
  }
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    get() { throw new Error('Storage disabled'); },
  });
  try {
    assert.equal(readLastSavedDisk(), null);
    assert.doesNotThrow(() => rememberSavedDisk('xila_drive.img'));
  } finally {
    delete globalThis.localStorage;
  }
});
