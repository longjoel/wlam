import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
const {chromium}=await import(process.env.WLAM_PLAYWRIGHT_MODULE??'playwright');
const browser=await chromium.launch({headless:true,args:['--no-sandbox']});
const page=await browser.newPage();
const leb=n=>{const bytes=[];do{let b=n&127;n>>>=7;bytes.push(b|(n?128:0));}while(n);return bytes;};
const name=s=>[...leb(s.length),...Buffer.from(s)];
const section=(id,body)=>[id,...leb(body.length),...body];
function moduleBytes(trap){
  const body=trap?[0,0,0x0b]:[0,0x41,0,0x41,0,0x10,0,0x1a,0x0b];
  return Buffer.from([0,97,115,109,1,0,0,0,
    ...section(1,[2,96,0,0,96,2,127,127,1,127]),
    ...section(2,[1,...name('TEST.dll'),...name('MissingApi'),0,1]),
    ...section(3,[1,0]),...section(5,[1,0,...leb(65536)]),
    ...section(7,[2,...name('entry'),0,1,...name('memory'),2,0]),
    ...section(10,[1,...leb(body.length),...body])]);
}
let trap=false;
try{
  await page.route('**/manifest.json',async route=>{
    const response=await route.fetch(),manifest=await response.json();
    manifest.imports.push({library:'TEST.dll',name:'MissingApi'});
    await route.fulfill({json:manifest});
  });
  await page.route('**/guest.wasm',route=>route.fulfill({body:moduleBytes(trap),contentType:'application/wasm'}));
  await page.goto(process.env.WLAM_TEST_URL??'http://127.0.0.1:8765/apps/web/');
  await page.click('#start-menu-button');
  await page.click('#menu-runner');
  const consoleAction=async selector=>{await page.locator('#task-app').click();await page.click(selector);};
  for(const isTrap of [false,true]){
    trap=isTrap;
    await consoleAction('#start');
    await page.waitForFunction(()=>document.querySelector('#status').textContent==='Failed'&&document.querySelector('#debug-report').textContent.includes('javascriptStack'));
    assert.equal(await page.locator('#debugger').getAttribute('open'),'');
    const downloadEvent=page.waitForEvent('download');
    await consoleAction('#download-debug');
    const download=await downloadEvent;
    const report=JSON.parse(await readFile(await download.path(),'utf8'));
    assert.equal(report.version,1);assert.equal(report.kind,'failure');
    assert.ok(report.runner.manifest);assert.ok(report.guest.registers.ESP);
    assert.ok(report.guest.stack.length<=32);assert.ok(report.guest.frames.length<=16);
    assert.ok(report.javascriptStack);
    assert.match(report.artifacts.guest.sha256,/^[a-f0-9]{64}$/);
    assert.match(report.artifacts.host.sha256,/^[a-f0-9]{64}$/);
    if(isTrap){assert.equal(report.failedApi,null);assert.match(report.reason,/unreachable/i);}
    else {assert.equal(report.failedApi.name,'MissingApi');assert.equal(report.failedApi.library,'TEST.dll');assert.match(await page.textContent('#error'),/TEST.dll!MissingApi/);}
  }
  console.log('Debug logs: actual worker missing API, Wasm trap, bounded guest snapshots, JSON download and restart passed');
}finally{await browser.close();}
