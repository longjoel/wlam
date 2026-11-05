import assert from 'node:assert/strict';

const { chromium } = await import(process.env.WLAM_PLAYWRIGHT_MODULE ?? 'playwright');
const browser = await chromium.launch({ headless: true, args: ['--no-sandbox'] });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
const errors = [];
page.on('pageerror', error => errors.push(error.message));

try {
  const url = new URL(process.env.WLAM_TEST_URL ?? 'http://127.0.0.1:8765/apps/web/');
  url.searchParams.set('renderer', 'webgl');
  url.searchParams.set('manifest', '/target/tolympic/bundle/manifest.json');
  await page.goto(url.href);
  assert.equal(await page.evaluate(() => crossOriginIsolated), true);

  const app = page.locator('#app-window');
  const canvas = page.locator('#screen');
  await page.evaluate(() => {
    window.__resizeDebug = [];
    for (const name of ['pointerdown', 'pointermove', 'pointerup']) {
      document.querySelector('#resize-grip').addEventListener(name, event => window.__resizeDebug.push(`${name}:${event.button}`));
    }
  });
  const startupGrip = await page.locator('#resize-grip').boundingBox();
  await page.mouse.move(startupGrip.x + startupGrip.width / 2, startupGrip.y + startupGrip.height / 2);
  await page.mouse.down();
  await page.mouse.move(startupGrip.x + startupGrip.width / 2 + 55, startupGrip.y + startupGrip.height / 2 + 35, { steps: 5 });
  await page.mouse.up();
  const requestedStartupSize = await page.evaluate(() => ({
    width: Number(document.querySelector('#guest-width').value),
    height: Number(document.querySelector('#guest-height').value),
  }));
  await page.click('#start');
  await page.waitForFunction(() => document.querySelector('#status').textContent === 'running');
  await page.waitForFunction(() => Number(document.querySelector('#screen').dataset.webglFrames) > 0);
  try {
    await page.waitForFunction(expected => {
      const target = document.querySelector('#screen');
      return target.width === expected.width && target.height === expected.height;
    }, requestedStartupSize, { timeout: 10000 });
  } catch (cause) {
    const state = await page.evaluate(() => ({
      requested: [document.querySelector('#guest-width').value, document.querySelector('#guest-height').value],
      actual: [document.querySelector('#screen').width, document.querySelector('#screen').height],
      appRect: document.querySelector('#app-window').getBoundingClientRect().toJSON(),
      gripRect: document.querySelector('#resize-grip').getBoundingClientRect().toJSON(),
      pointerEvents: window.__resizeDebug,
      status: document.querySelector('#status').textContent,
      error: document.querySelector('#error').textContent,
    }));
    throw new Error(`Pre-start desktop resize was not applied: ${JSON.stringify(state)}`, { cause });
  }

  const beforeMove = await app.boundingBox();
  const title = await page.locator('#titlebar').boundingBox();
  await page.mouse.move(title.x + title.width / 2, title.y + title.height / 2);
  await page.mouse.down();
  await page.mouse.move(title.x + title.width / 2 + 55, title.y + title.height / 2 + 35, { steps: 5 });
  await page.mouse.up();
  const afterMove = await app.boundingBox();
  assert.ok(afterMove.x > beforeMove.x + 40 && afterMove.y > beforeMove.y + 20, 'title bar drag should move the runner window');

  await page.click('#window-minimize');
  assert.ok(await app.evaluate(element => element.classList.contains('is-minimized')));
  await page.click('#task-app');
  assert.ok(!(await app.evaluate(element => element.classList.contains('is-minimized'))), 'taskbar should restore a minimized window');

  const initialCanvas = await canvas.evaluate(element => ({ width: element.width, height: element.height }));
  await page.click('#window-maximize');
  assert.ok(await app.evaluate(element => element.classList.contains('is-maximized')));
  await page.waitForFunction(previous => {
    const target = document.querySelector('#screen');
    return target.width !== previous.width || target.height !== previous.height;
  }, initialCanvas, { timeout: 10000 });
  await page.click('#window-maximize');
  assert.ok(!(await app.evaluate(element => element.classList.contains('is-maximized'))), 'maximize button should restore the window');

  const beforeResize = await canvas.evaluate(element => ({ width: element.width, height: element.height }));
  const grip = await page.locator('#resize-grip').boundingBox();
  await page.mouse.move(grip.x + grip.width / 2, grip.y + grip.height / 2);
  await page.mouse.down();
  await page.mouse.move(grip.x + grip.width / 2 + 70, grip.y + grip.height / 2 + 45, { steps: 5 });
  await page.mouse.up();
  await page.waitForFunction(previous => {
    const target = document.querySelector('#screen');
    return target.width !== previous.width && target.height !== previous.height;
  }, beforeResize, { timeout: 10000 });

  const beforeKeyboardResize = await canvas.evaluate(element => ({ width: element.width, height: element.height }));
  await page.locator('#resize-grip').focus();
  await page.keyboard.press('ArrowRight');
  await page.keyboard.press('ArrowDown');
  await page.waitForFunction(previous => {
    const target = document.querySelector('#screen');
    return target.width > previous.width && target.height > previous.height;
  }, beforeKeyboardResize, { timeout: 10000 });

  const beforeViewportResize = await canvas.evaluate(element => ({ width: element.width, height: element.height }));
  await page.setViewportSize({ width: 760, height: 600 });
  await page.waitForFunction(() => {
    const rect = document.querySelector('#app-window').getBoundingClientRect();
    return rect.left >= 0 && rect.top >= 0 && rect.right <= innerWidth && rect.bottom <= innerHeight - 48;
  });
  await page.waitForFunction(previous => {
    const target = document.querySelector('#screen');
    return target.width !== previous.width && target.height !== previous.height;
  }, beforeViewportResize, { timeout: 10000 });

  await page.click('#start-menu-button');
  assert.ok(await page.locator('#start-menu').evaluate(element => element.classList.contains('open')));
  await page.click('#menu-runner');
  assert.ok(!(await page.locator('#start-menu').evaluate(element => element.classList.contains('open'))));
  assert.deepEqual(errors, []);
  console.log('Simulated desktop passed: title bar drag, minimize/taskbar restore, maximize/restore, pointer and keyboard guest resize, viewport resize, and Start menu.');
  await page.screenshot({ path: '/tmp/wlam-fake-desktop.png' });
} finally {
  await page.locator('#stop').click().catch(() => {});
  await browser.close();
}
