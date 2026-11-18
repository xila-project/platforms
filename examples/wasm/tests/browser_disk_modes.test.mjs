import assert from 'node:assert/strict';
import test from 'node:test';

const exampleAddress = process.env.XILA_BROWSER_EXAMPLE_ADDRESS;
const chromiumExecutable = process.env.XILA_CHROMIUM_EXECUTABLE;

test('memory boots independently and the disk panel switches and rebuilds saved disks',
  { skip: !exampleAddress, timeout: 90_000 }, async () => {
    const { chromium } = await import(process.env.XILA_PLAYWRIGHT_MODULE ?? 'playwright');
    const browser = await chromium.launch({ executablePath: chromiumExecutable });
    try {
      const context = await browser.newContext();
      const legacyDisk = 'xila_drive.img';
      const page = await context.newPage();
      await page.addInitScript((legacyName) => {
        localStorage.setItem('xila.selectedDisk', legacyName);
        window.storageRequests = 0;
        window.lockRequests = 0;
        const getDirectory = navigator.storage.getDirectory.bind(navigator.storage);
        const requestLock = navigator.locks.request.bind(navigator.locks);
        navigator.storage.getDirectory = (...argumentsList) => {
          window.storageRequests++;
          return getDirectory(...argumentsList);
        };
        navigator.locks.request = (...argumentsList) => {
          window.lockRequests++;
          return requestLock(...argumentsList);
        };
      }, legacyDisk);

      await page.goto(exampleAddress);
      const status = page.getByRole('status');
      await status.filter({ hasText: 'Using Memory.' }).waitFor();
      assert.deepEqual(await page.evaluate(() => [window.storageRequests, window.lockRequests]), [0, 0]);
      assert.deepEqual(await page.evaluate(async () => {
        const root = await navigator.storage.getDirectory();
        const files = [];
        for await (const name of root.keys()) files.push(name);
        return files;
      }), []);

      const secondTab = await context.newPage();
      await secondTab.goto(exampleAddress);
      await secondTab.getByRole('status').filter({ hasText: 'Using Memory.' }).waitFor();
      await secondTab.close();

      await page.getByRole('button', { name: 'Toggle Help Panel' }).click();
      const diskList = page.getByRole('listbox', { name: 'Available Xila disks' });
      assert.equal(await diskList.inputValue(), 'memory');
      assert.equal(await diskList.getByRole('option').first().textContent(), 'Memory (resets on reload)');
      await page.evaluate(async () => {
        const root = await navigator.storage.getDirectory();
        await root.getFileHandle('xila_drive.img', { create: true });
      });
      // Reopen the panel to pick up the legacy disk created by another page.
      await page.getByRole('button', { name: 'Toggle Help Panel' }).click();
      await page.getByRole('button', { name: 'Toggle Help Panel' }).click();
      await diskList.getByRole('option', { name: /xila_drive\.img/ }).waitFor({ state: 'attached' });

      page.once('dialog', (dialog) => dialog.accept());
      await page.getByRole('button', { name: 'New disk' }).click();
      await status.filter({ hasText: /^Using xila_\d{4}-\d{2}-\d{2}.*\.img\.$/ }).waitFor();
      const savedDisk = (await status.textContent()).slice('Using '.length, -1);
      const seedVersion = await page.evaluate((name) => localStorage.getItem(`xila.seedVersion.${name}`), savedDisk);
      assert.match(seedVersion, /^[0-9a-f]{64}$/);
      await page.getByRole('button', { name: 'Toggle Help Panel' }).click();
      await diskList.selectOption('memory');
      page.once('dialog', (dialog) => dialog.accept());
      await page.getByRole('button', { name: 'Use selected disk' }).click();
      await status.filter({ hasText: 'Using Memory.' }).waitFor();

      await page.evaluate(async (name) => {
        localStorage.setItem(`xila.seedVersion.${name}`, 'older');
        const root = await navigator.storage.getDirectory();
        const disk = await root.getFileHandle(name);
        const stream = await disk.createWritable();
        await stream.write(new Uint8Array(16 * 1024 * 1024));
        await stream.close();
      }, savedDisk);
      await page.getByRole('button', { name: 'Toggle Help Panel' }).click();
      await diskList.getByRole('option', { name: `${savedDisk} (older seed version)` }).waitFor({ state: 'attached' });
      await diskList.selectOption(savedDisk);
      const rebuild = page.getByRole('button', { name: 'Rebuild from the current version' });
      await rebuild.waitFor();
      page.once('dialog', (dialog) => dialog.accept());
      await page.getByRole('button', { name: 'Use selected disk' }).click();
      await status.filter({ hasText: `Could not mount ${savedDisk}` }).waitFor();

      page.once('dialog', (dialog) => dialog.dismiss());
      await rebuild.click();
      assert.match(await status.textContent(), /Could not mount/);
      assert.equal(await page.evaluate((name) => localStorage.getItem(`xila.seedVersion.${name}`), savedDisk), 'older');

      page.once('dialog', (dialog) => dialog.accept());
      await rebuild.click();
      await status.filter({ hasText: `Using ${savedDisk}.` }).waitFor();
      assert.equal(await page.evaluate((name) => localStorage.getItem(`xila.seedVersion.${name}`), savedDisk), seedVersion);
    } finally {
      await browser.close();
    }
  });
