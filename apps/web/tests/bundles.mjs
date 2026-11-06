import assert from 'node:assert/strict';

const { chromium } = await import(process.env.WLAM_PLAYWRIGHT_MODULE ?? 'playwright');
const browser = await chromium.launch({ headless: true, args: ['--no-sandbox'] });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
const errors = [];
page.on('pageerror', error => errors.push(error.message));
const visibleTasks = () => page.locator('#taskbar .task-app:visible').count();

try {
  const url = new URL(process.env.WLAM_TEST_URL ?? 'http://127.0.0.1:8765/apps/web/');
  await page.goto(url.href);

  const catalog = await page.evaluate(async () => {
    const response = await fetch(new URL('./bundles.json', location.href));
    return response.ok ? response.json() : { bundles: [] };
  });
  assert.ok(catalog.bundles.length > 0, 'the dev server should discover built bundles');
  assert.ok(catalog.bundles.every(bundle => bundle.manifest.endsWith('/bundle/manifest.json')));
  await page.waitForFunction(() => document.querySelectorAll('#desktop-icons [data-bundle]').length > 0);
  const shortcuts = await page.evaluate(() => [...document.querySelectorAll('#desktop-icons [data-bundle]')].map(button => button.dataset.bundle));
  assert.deepEqual(shortcuts, catalog.bundles.map(bundle => bundle.id));
  assert.equal(await page.locator('.app-window:visible').count(), 0, 'the desktop starts empty');
  assert.equal(await visibleTasks(), 0);

  // The interactive bundle follows exactly the same path as every other one:
  // a bare guest window, no developer chrome.
  await page.click('[data-bundle="interactive"]');
  await page.waitForFunction(() => {
    const element = document.querySelector('.guest-window');
    return element && /^(running|waiting)$/.test(element.dataset.state);
  }, null, { timeout: 20000 });
  assert.equal(await page.locator('.guest-window').count(), 1);
  assert.equal(await page.locator('.guest-window #toolbar, .guest-window #debugger, .guest-window [data-role="manifest"]').count(), 0);
  assert.equal(await visibleTasks(), 1);
  assert.equal(await page.locator('#app-window').isVisible(), false, 'the developer console stays closed');

  await page.locator('.guest-window [data-role="window-close"]').click();
  await page.waitForFunction(() => document.querySelectorAll('.guest-window').length === 0);
  assert.equal(await visibleTasks(), 0);

  const other = shortcuts.includes('tolympic') ? 'tolympic' : shortcuts.find(id => id !== 'interactive');
  if (other) {
    await page.click(`[data-bundle="${other}"]`);
    await page.waitForFunction(() => document.querySelectorAll('.guest-window').length === 1);
    assert.equal(await visibleTasks(), 1);
    const second = page.locator('.guest-window');
    assert.equal(await second.locator('[data-role="screen"]').count(), 1);
    assert.ok(await second.locator('[data-role="screen"]').evaluate(canvas => canvas.clientWidth > 100));
    await page.waitForFunction(() => /^(running|waiting|Exited|Failed)$/.test(document.querySelector('.guest-window').dataset.state));
    if (other === 'tolympic') {
      await page.waitForFunction(() => Number(document.querySelector('.guest-window canvas').dataset.webglFrames) > 0, null, { timeout: 20000 });
      const dimensions = await second.evaluate(element => {
        const canvas = element.querySelector('canvas');
        const wrap = element.querySelector('[data-role="screen-wrap"]');
        return { canvas: [canvas.width, canvas.height], wrap: [wrap.clientWidth, wrap.clientHeight] };
      });
      assert.ok(Math.abs(dimensions.canvas[0] - dimensions.wrap[0]) <= 2 && Math.abs(dimensions.canvas[1] - dimensions.wrap[1]) <= 2, `guest surface should match the window: ${JSON.stringify(dimensions)}`);
    }
    await second.locator('[data-role="window-minimize"]').click();
    assert.ok(await second.evaluate(element => element.classList.contains('is-minimized')));
    assert.ok(await page.locator('#taskbar .task-app:visible').evaluate(element => !element.classList.contains('active')));
    await page.locator('#taskbar .task-app:visible').click();
    assert.ok(!(await second.evaluate(element => element.classList.contains('is-minimized'))));
    await second.locator('[data-role="window-close"]').click();
    await page.waitForFunction(() => document.querySelectorAll('.guest-window').length === 0);
    assert.equal(await visibleTasks(), 0);
  }

  assert.deepEqual(errors, []);
  console.log(`Bundle shortcuts passed: ${shortcuts.join(', ')}${other ? `; ${other} opened in its own bare window` : ''}.`);
  await page.screenshot({ path: '/tmp/wlam-bundle-desktop.png' });
} finally {
  await browser.close();
}
