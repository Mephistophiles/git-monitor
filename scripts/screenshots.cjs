/* npm install --no-save playwright@1.62.1; node scripts/screenshots.cjs */
const { chromium } = require('playwright');
const path = require('node:path');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const { pathToFileURL } = require('node:url');

(async () => {
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH || undefined,
    args: ['--no-sandbox'],
  });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1160 }, deviceScaleFactor: 1 });
  const errors = [], requests = [];
  page.on('pageerror', e => errors.push(String(e)));
  await page.route(/^https?:/, route => { requests.push(route.request().url()); return route.abort(); });
  await page.goto(pathToFileURL(path.resolve('docs/example-report.html')).href);
  await page.evaluate(() => document.fonts.ready);
  assert.ok(await page.locator('.file').count() > 0);
  const exportButton = page.locator('[data-export]').first();
  const expected = Buffer.from(await exportButton.getAttribute('data-export'), 'base64');
  const downloadEvent = page.waitForEvent('download');
  await exportButton.click();
  const downloaded = await downloadEvent;
  assert.deepEqual(await fs.readFile(await downloaded.path()), expected);
  await page.screenshot({ path: 'docs/report-unified.png' });
  await page.getByRole('button', { name: 'Side by side', exact: true }).click();
  assert.equal(await page.locator('html').getAttribute('data-view'), 'split');
  await page.screenshot({ path: 'docs/report-side-by-side.png' });
  await page.getByRole('button', { name: 'Switch theme' }).click();
  assert.equal(await page.locator('html').getAttribute('data-theme'), 'dark');
  await page.getByRole('searchbox').fill('allocation.c');
  assert.equal(await page.locator('.file:not([hidden])').count(), 3);
  await page.getByRole('searchbox').fill('this-path-does-not-exist');
  assert.equal(await page.locator('.file:not([hidden])').count(), 0);
  const offline = await browser.newContext({ javaScriptEnabled: false, viewport: { width: 390, height: 844 } });
  const noJS = await offline.newPage();
  await noJS.goto(pathToFileURL(path.resolve('docs/example-report.html')).href);
  assert.ok(await noJS.locator('.unified-view').first().isVisible());
  assert.ok((await noJS.locator('body').innerText()).includes('Guard allocation alignment'));
  assert.deepEqual(errors, []);
  assert.deepEqual(requests, []);
  await browser.close();
  console.log('Screenshots generated; layout, theme, search, offline, and no-JS checks passed.');
})().catch(e => { console.error(e); process.exit(1); });
