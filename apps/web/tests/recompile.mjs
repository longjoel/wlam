import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {rmSync} from 'node:fs';

const {chromium} = await import(process.env.WLAM_PLAYWRIGHT_MODULE ?? 'playwright');
const browser = await chromium.launch({headless: true, args: ['--no-sandbox']});
const page = await browser.newPage({viewport: {width: 1280, height: 900}});
const errors = [];
page.on('pageerror', error => errors.push(error.message));
const archive = 'target/recompile-wizard-test.zip';
let bundleDirectory = null;

try {
  // Use the real interactive fixture so the generated bundle can be launched.
  execFileSync('python3', ['-c', `
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], 'w', zipfile.ZIP_DEFLATED) as package:
    package.write(sys.argv[2], 'demo/window.exe')
    package.writestr('demo/data/hello.txt', 'hello from the wizard')
`, archive, 'target/interactive/window.exe']);

  const url = new URL(process.env.WLAM_TEST_URL ?? 'http://127.0.0.1:8765/apps/web/');
  await page.goto(url.href);
  await page.waitForFunction(() => !document.querySelector('#desktop-recompile').hidden, null, {timeout: 10000});

  // Drag the ZIP onto the desktop and check the drop overlay, then the wizard.
  const dataTransfer = await page.evaluateHandle(async zip => {
    const bytes = await (await fetch(zip)).arrayBuffer();
    const transfer = new DataTransfer();
    transfer.items.add(new File([bytes], 'Wizard Test.zip', {type: 'application/zip'}));
    return transfer;
  }, new URL(`/${archive}`, url).href);
  await page.dispatchEvent('#desktop', 'dragenter', {dataTransfer});
  assert.equal(await page.locator('#drop-overlay').isVisible(), true);
  await page.dispatchEvent('#desktop', 'drop', {dataTransfer});
  assert.equal(await page.locator('#drop-overlay').isVisible(), false);
  await page.waitForFunction(() => document.querySelector('#recompile-dialog').open
    && document.querySelectorAll('#recompile-exe option').length > 0, null, {timeout: 10000});
  assert.equal(await page.inputValue('#recompile-exe'), 'demo/window.exe');
  assert.match(await page.textContent('#recompile-file'), /2 entries/);

  await page.click('#recompile-next');
  await page.waitForFunction(() => /No blocking imports/.test(document.querySelector('#recompile-analysis').textContent), null, {timeout: 20000});
  assert.match(await page.textContent('#recompile-analysis'), /Imports/);
  assert.equal(await page.locator('#recompile-next').isDisabled(), false);
  await page.click('#recompile-next');

  await page.waitForFunction(() => document.querySelector('#recompile-steps [data-step="report"]').classList.contains('active'), null, {timeout: 60000});
  assert.equal(await page.textContent('#recompile-next'), 'Open on desktop');
  const reportHref = await page.getAttribute('#recompile-report .report-links a', 'href');
  assert.ok(reportHref?.endsWith('/report.json'), reportHref);
  const report = await page.evaluate(async href => (await fetch(href)).json(), reportHref);
  assert.equal(report.kind, 'wlam-recompile-report');
  assert.equal(report.source.executable, 'demo/window.exe');
  assert.deepEqual(report.assets.map(asset => asset.path), ['C:\\demo\\data\\hello.txt']);
  assert.ok(report.bundle.artifacts.some(artifact => artifact.path === 'guest.wasm'));
  assert.ok(report.bundle.artifacts.some(artifact => artifact.path === 'compatibility.md'));
  assert.match(report.matrix, /Compatibility matrix/);

  const id = report.bundle.id;
  bundleDirectory = `target/${id}`;
  assert.equal(await page.locator(`[data-bundle="${id}"]`).count(), 1, 'the recompiled shortcut should be on the desktop');
  await page.reload();
  await page.waitForFunction(bundle => document.querySelector(`[data-bundle="${bundle}"]`) !== null, id, {timeout: 10000});

  // Opening the shortcut proves the published manifest and module load in the
  // ordinary desktop path, not a special wizard preview.
  await page.click(`[data-bundle="${id}"]`);
  await page.waitForFunction(() => {
    const element = document.querySelector('.guest-window');
    return element && /^(running|waiting)$/.test(element.dataset.state);
  }, null, {timeout: 20000});
  await page.locator('.guest-window [data-role="window-close"]').click();
  await page.waitForFunction(() => document.querySelectorAll('.guest-window').length === 0);

  assert.deepEqual(errors, []);
  console.log(`Recompile wizard passed: ${id} produced a launchable bundle and report.`);
  await page.screenshot({path: '/tmp/wlam-recompile-wizard.png'});
} finally {
  await browser.close();
  rmSync(archive, {force: true});
  if (bundleDirectory) rmSync(bundleDirectory, {recursive: true, force: true});
}
