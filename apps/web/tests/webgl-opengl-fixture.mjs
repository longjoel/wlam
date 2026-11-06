import assert from 'node:assert/strict';

const { chromium } = await import(process.env.WLAM_PLAYWRIGHT_MODULE ?? 'playwright');
const browser = await chromium.launch({ headless: true, args: ['--no-sandbox'] });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', error => errors.push(error.message));

try {
  const url = new URL(process.env.WLAM_TEST_URL ?? 'http://127.0.0.1:8765/apps/web/');
  url.searchParams.set('renderer', 'webgl');
  url.searchParams.set('manifest', '/target/opengl-fixture/bundle/manifest.json');
  await page.goto(url.href);
  await page.locator('#task-app').click();
  await page.click('#start');
  await page.waitForFunction(() => document.querySelector('#status').textContent === 'Exited (0)', null, { timeout: 15000 });
  const pixels = await page.locator('#screen').evaluate(canvas => {
    const gl = canvas.getContext('webgl');
    const read = (x, y) => {
      const rgba = new Uint8Array(4);
      gl.readPixels(x, y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, rgba);
      return [...rgba];
    };
    const image = new Uint8Array(canvas.width * canvas.height * 4);
    gl.readPixels(0, 0, canvas.width, canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, image);
    const greenPoints = [];
    for (let y = 0; y < canvas.height; y++) for (let x = 0; x < canvas.width; x++) {
      const at = (y * canvas.width + x) * 4;
      if (image[at] === 0 && image[at + 1] === 255 && image[at + 2] === 0) greenPoints.push([x, y]);
    }
    return {
      point: greenPoints,
      triangle: read(16, 8),
      background: read(0, 0),
    };
  });
  assert.ok(pixels.point.length > 0, `GL_POINTS should produce a green pixel, got ${JSON.stringify(pixels.point)}`);
  assert.ok(pixels.point.some(([x, y]) => Math.abs(x - 16) <= 1 && Math.abs(y - 12) <= 1),
    `GL_POINTS should rasterize around the clip-space center, got ${JSON.stringify(pixels.point)}`);
  assert.deepEqual(pixels.triangle, [0, 0, 255, 255]);
  assert.deepEqual(pixels.background, [0, 0, 0, 255]);
  assert.deepEqual(errors, []);
  console.log('WebGL OpenGL fixture passed: ordered triangle and GL_POINTS draws render visible pixels.');
} finally {
  await browser.close();
}
