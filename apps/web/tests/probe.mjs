// Diagnose a real bundle in Chromium without claiming gameplay acceptance.
import {mkdir} from 'node:fs/promises';
const {chromium}=await import(process.env.WLAM_PLAYWRIGHT_MODULE??'playwright');
const browser=await chromium.launch({headless:true,args:['--no-sandbox']});
try {
  const page=await browser.newPage();
  page.on('pageerror',error=>console.log('PAGE_ERROR',error.message));
  await page.goto(process.env.WLAM_TEST_URL??'http://127.0.0.1:8765/apps/web/');
  await page.fill('#manifest',process.env.WLAM_PROBE_MANIFEST??'/target/hover/bundle/manifest.json');
  await page.click('#start');
  await page.waitForFunction(()=>/Failed|waiting|Exited|Waiting for dialog/.test(document.querySelector('#status').textContent),{},{timeout:60000});
  if(await page.locator('#guest-dialog').isVisible()){
    console.log('DIALOG',await page.textContent('#dialog-text'));
    await page.screenshot({path:'target/hover/dialog.png'});
    await page.getByRole('button',{name:'OK',exact:true}).click();
    await page.waitForFunction(()=>/Failed|waiting|Exited/.test(document.querySelector('#status').textContent),{},{timeout:60000});
  }
  console.log(JSON.stringify(await page.evaluate(()=>({
    status:document.querySelector('#status').textContent,
    error:document.querySelector('#error').textContent,
    width:document.querySelector('#screen').width,
    height:document.querySelector('#screen').height,
  })),null,2));
  await mkdir('target/hover',{recursive:true});
  await page.screenshot({path:'target/hover/browser.png'});
  await page.click('#stop');
} finally { await browser.close(); }
