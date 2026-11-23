import assert from 'node:assert/strict';
import test from 'node:test';

const exampleAddress = process.env.XILA_BROWSER_EXAMPLE_ADDRESS;
const chromiumExecutable = process.env.XILA_CHROMIUM_EXECUTABLE;

test('the loading page and bootsplash have the same background',
  { skip: !exampleAddress, timeout: 30_000 }, async () => {
    const { chromium } = await import(process.env.XILA_PLAYWRIGHT_MODULE ?? 'playwright');
    const browser = await chromium.launch({ executablePath: chromiumExecutable });
    try {
      const page = await browser.newPage({ colorScheme: 'light' });
      let loadingBackground;
      await page.route('**/seed.img', async route => {
        loadingBackground = await page.evaluate(() =>
          getComputedStyle(document.documentElement).backgroundColor);
        await route.continue();
      });
      await page.goto(exampleAddress);
      await page.waitForFunction(() => {
        const canvas = document.querySelector('#canvas-screen');
        if (!canvas) return false;
        const [red, green, blue] = canvas.getContext('2d').getImageData(0, 0, 1, 1).data;
        return red === 9 && green === 9 && blue === 11;
      });
      assert.equal(loadingBackground, 'rgb(9, 9, 11)');
    } finally {
      await browser.close();
    }
  });
