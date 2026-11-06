import assert from 'node:assert/strict';

const { chromium } = await import(process.env.WLAM_PLAYWRIGHT_MODULE ?? 'playwright');
const browser = await chromium.launch({ headless: true, args: ['--no-sandbox'] });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
const errors = [];
page.on('pageerror', error => errors.push(error.message));

try {
  const url = new URL(process.env.WLAM_TEST_URL ?? 'http://127.0.0.1:8765/apps/web/');
  await page.goto(url.href);
  assert.equal(await page.evaluate(() => crossOriginIsolated), true);

  // Every bundle shortcut opens the same bare guest window and starts it.
  await page.waitForSelector('[data-bundle="tolympic"]');
  await page.click('[data-bundle="tolympic"]');
  const app = page.locator('.guest-window');
  const canvas = app.locator('[data-role="screen"]');
  await page.waitForFunction(() => Number(document.querySelector('.guest-window [data-role="screen"]').dataset.webglFrames) > 0, null, { timeout: 20000 });
  assert.equal(await app.locator('#toolbar, #debugger, [data-role="manifest"]').count(), 0, 'bundle windows contain only the guest display');

  const guestSize = () => canvas.evaluate(element => ({ width: element.width, height: element.height }));
  await page.waitForFunction(() => {
    const element = document.querySelector('.guest-window');
    const target = element.querySelector('[data-role="screen"]');
    const wrap = element.querySelector('[data-role="screen-wrap"]');
    return target.width === wrap.clientWidth && target.height === wrap.clientHeight;
  }, null, { timeout: 10000 });

  const beforeMove = await app.boundingBox();
  const title = await app.locator('[data-role="titlebar"]').boundingBox();
  await page.mouse.move(title.x + title.width / 2, title.y + title.height / 2);
  await page.mouse.down();
  await page.mouse.move(title.x + title.width / 2 + 55, title.y + title.height / 2 + 35, { steps: 5 });
  await page.mouse.up();
  const afterMove = await app.boundingBox();
  assert.ok(afterMove.x > beforeMove.x + 40 && afterMove.y > beforeMove.y + 20, 'title bar drag should move the guest window');

  await app.locator('[data-role="window-minimize"]').click();
  assert.ok(await app.evaluate(element => element.classList.contains('is-minimized')));
  await page.locator('#taskbar .task-app:visible').click();
  assert.ok(!(await app.evaluate(element => element.classList.contains('is-minimized'))), 'taskbar should restore a minimized window');

  const initialCanvas = await guestSize();
  await app.locator('[data-role="window-maximize"]').click();
  assert.ok(await app.evaluate(element => element.classList.contains('is-maximized')));
  await page.waitForFunction(previous => {
    const target = document.querySelector('.guest-window [data-role="screen"]');
    return target.width !== previous.width || target.height !== previous.height;
  }, initialCanvas, { timeout: 10000 });
  await app.locator('[data-role="window-maximize"]').click();
  assert.ok(!(await app.evaluate(element => element.classList.contains('is-maximized'))), 'maximize button should restore the window');

  const beforeResize = await guestSize();
  const grip = await app.locator('[data-role="resize-grip"]').boundingBox();
  await page.mouse.move(grip.x + grip.width / 2, grip.y + grip.height / 2);
  await page.mouse.down();
  await page.mouse.move(grip.x + grip.width / 2 + 70, grip.y + grip.height / 2 + 45, { steps: 5 });
  await page.mouse.up();
  await page.waitForFunction(previous => {
    const target = document.querySelector('.guest-window [data-role="screen"]');
    return target.width !== previous.width && target.height !== previous.height;
  }, beforeResize, { timeout: 10000 });

  const beforeKeyboardResize = await guestSize();
  await app.locator('[data-role="resize-grip"]').focus();
  await page.keyboard.press('ArrowRight');
  await page.keyboard.press('ArrowDown');
  await page.waitForFunction(previous => {
    const target = document.querySelector('.guest-window [data-role="screen"]');
    return target.width > previous.width && target.height > previous.height;
  }, beforeKeyboardResize, { timeout: 10000 });

  const beforeViewportResize = await guestSize();
  await page.setViewportSize({ width: 760, height: 600 });
  await page.waitForFunction(() => {
    const rect = document.querySelector('.guest-window').getBoundingClientRect();
    return rect.left >= 0 && rect.top >= 0 && rect.right <= innerWidth && rect.bottom <= innerHeight - 48;
  });
  await page.waitForFunction(previous => {
    const target = document.querySelector('.guest-window [data-role="screen"]');
    return target.width !== previous.width && target.height !== previous.height;
  }, beforeViewportResize, { timeout: 10000 });

  // The guest surface tracks the window after every desktop operation.
  assert.ok(await page.evaluate(() => {
    const element = document.querySelector('.guest-window');
    const target = element.querySelector('[data-role="screen"]');
    const wrap = element.querySelector('[data-role="screen-wrap"]');
    return target.width === wrap.clientWidth && target.height === wrap.clientHeight;
  }));

  await page.click('#start-menu-button');
  assert.ok(await page.locator('#start-menu').evaluate(element => element.classList.contains('open')));
  await page.click('#menu-runner');
  assert.ok(!(await page.locator('#start-menu').evaluate(element => element.classList.contains('open'))));
  assert.ok(await page.locator('#app-window').isVisible(), 'the Start menu should open the developer console');
  assert.deepEqual(errors, []);
  console.log('Simulated desktop passed: bare guest shortcut, title bar drag, minimize/taskbar restore, maximize/restore, pointer and keyboard guest resize, viewport resize, and Start menu.');
  await page.screenshot({ path: '/tmp/wlam-fake-desktop.png' });
} finally {
  await browser.close();
}
