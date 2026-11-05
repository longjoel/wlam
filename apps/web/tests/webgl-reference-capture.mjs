import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';

const { chromium } = await import(process.env.WLAM_PLAYWRIGHT_MODULE ?? 'playwright');
const browser = await chromium.launch({ headless: true, args: ['--no-sandbox'] });
const page = await browser.newPage();
const base = process.env.WLAM_TEST_URL ?? 'http://127.0.0.1:8765/apps/web/';
const url = new URL(base);
url.searchParams.set('renderer', 'webgl');
url.searchParams.set('manifest', '/target/tolympic/bundle/manifest.json');

async function waitFrame(frame) {
  await page.waitForFunction(target => Number(document.querySelector('#screen').dataset.webglFrames) >= target,
    frame, { timeout: 45000 });
}

try {
  await page.goto(url.href);
  assert.equal(await page.evaluate(() => crossOriginIsolated), true);
  await page.click('#start');
  await waitFrame(1);
  await page.locator('#screen').focus();
  await page.keyboard.press('Space');
  await waitFrame(70);
  await page.keyboard.press('b');
  await waitFrame(80);
  await page.locator('#guest-width').fill('600');
  await page.locator('#guest-height').fill('450');
  await page.click('#resize-guest');
  await page.waitForFunction(() => document.querySelector('#screen').width === 600
    && document.querySelector('#screen').height === 450);
  await waitFrame(90);
  const dataUrl = await page.locator('#screen').evaluate(canvas => canvas.toDataURL('image/png'));
  const output = process.env.WLAM_WEBGL_CAPTURE ?? '/tmp/tolympic-webgl-frame90.png';
  await writeFile(output, Buffer.from(dataUrl.split(',')[1], 'base64'));
  console.log(`Saved WebGL frame 90 (600×450) to ${output}.`);
} finally {
  await browser.close();
}
