import assert from 'node:assert/strict';

const { chromium } = await import(process.env.WLAM_PLAYWRIGHT_MODULE ?? 'playwright');
const browser = await chromium.launch({ headless: true, args: ['--no-sandbox'] });
const page = await browser.newPage();
const pageErrors = [];
page.on('pageerror', error => pageErrors.push(error.message));
const base = process.env.WLAM_TEST_URL ?? 'http://127.0.0.1:8765/apps/web/';
const url = new URL(base);
url.searchParams.set('renderer', 'webgl');
url.searchParams.set('manifest', process.env.WLAM_WEBGL_MANIFEST ?? '/target/tolympic/bundle/manifest.json');

async function waitStatus(expected) {
  await page.waitForFunction(value => document.querySelector('#status').textContent === value, expected);
}
const focusConsole = () => page.locator('#task-app').click();
const consoleAction = async selector => { await focusConsole(); await page.click(selector); };

try {
  await page.goto(url.href);
  assert.equal(await page.evaluate(() => crossOriginIsolated), true);
  await consoleAction('#start');
  await waitStatus('running');
  await page.waitForFunction(() => document.querySelector('#screen')?.getContext('webgl') !== null);
  await page.waitForFunction(() => Number(document.querySelector('#screen').dataset.webglFrames) > 0, null, { timeout: 15000 });
  const initialFrameCount = await page.locator('#screen').evaluate(canvas => Number(canvas.dataset.webglFrames));
  await page.waitForTimeout(2000);
  const laterFrameCount = await page.locator('#screen').evaluate(canvas => Number(canvas.dataset.webglFrames));
  assert.ok(laterFrameCount > initialFrameCount, 'WebGL should keep presenting frames without manual stepping.');
  console.log(`WebGL presented ${laterFrameCount - initialFrameCount} additional frames in 2 seconds.`);
  assert.equal(await page.locator('#error').innerText(), '');
  await page.screenshot({ path: '/tmp/tolympic-webgl-rings.png' });

  await page.locator('#screen').focus();
  await page.keyboard.press('Space');
  await focusConsole();
  await page.locator('#guest-width').fill('600');
  await page.locator('#guest-height').fill('450');
  await page.click('#resize-guest');
  try {
    await page.waitForFunction(() => document.querySelector('#screen').width === 600
      && document.querySelector('#screen').height === 450, null, { timeout: 10000 });
  } catch (cause) {
    const state = await page.evaluate(() => ({
      width: document.querySelector('#screen').width,
      height: document.querySelector('#screen').height,
      frames: document.querySelector('#screen').dataset.webglFrames,
      status: document.querySelector('#status').textContent,
      error: document.querySelector('#error').textContent,
    }));
    throw new Error(`WebGL resize did not present: ${JSON.stringify(state)}`, { cause });
  }
  await page.keyboard.press('b');
  await page.keyboard.press('d');
  await page.waitForTimeout(300);
  await consoleAction('#stop');
  await waitStatus('Stopped');

  await consoleAction('#start');
  await waitStatus('running');
  const stoppedFrameCount = await page.locator('#screen').evaluate(canvas => Number(canvas.dataset.webglFrames));
  await page.waitForFunction(previous => Number(document.querySelector('#screen').dataset.webglFrames) > previous,
    stoppedFrameCount, { timeout: 15000 });
  await page.locator('#screen').focus();
  await page.keyboard.press('Escape');
  await waitStatus('Exited (0)');
  assert.deepEqual(pageErrors, []);
  assert.equal(await page.locator('#error').innerText(), '');
  console.log('TOLYMPIC WebGL UI passed: shader draws, resize, Space/b/d input, Stop/Restart, Escape and clean exit.');
} finally {
  await browser.close();
}
