import assert from 'node:assert/strict';
const {chromium}=await import(process.env.WLAM_PLAYWRIGHT_MODULE??'playwright');
const browser=await chromium.launch({headless:true,args:['--no-sandbox']});
const page=await browser.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));
const url=process.env.WLAM_TEST_URL??'http://127.0.0.1:8765/apps/web/';
async function pixel(rgb){await page.waitForFunction(expected=>{const canvas=document.querySelector('#screen');const p=canvas.getContext('2d').getImageData(0,0,canvas.width,canvas.height).data;return p.length===canvas.width*canvas.height*4&&p.length>0&&p.every((v,i)=>v===expected[i%4]);},[...rgb,255]);}
async function openConsole(){await page.click('#start-menu-button');await page.click('#menu-runner');}
async function consoleAction(selector){await page.locator('#task-app').click();await page.click(selector);}
async function status(text){await page.waitForFunction(value=>document.querySelector('#status').textContent===value,text);}
try{
  await page.goto(url);assert.equal(await page.evaluate(()=>crossOriginIsolated),true);
  await openConsole();
  await consoleAction('#start');await pixel([0x12,0x34,0x56]);await status('waiting');
  await page.screenshot({path:'target/interactive/browser.png'});
  // A main-thread timer must fire while the guest is blocked in GetMessage.
  assert.equal(await page.evaluate(()=>new Promise(resolve=>setTimeout(()=>resolve('responsive'),30))),'responsive');
  try{await page.click('#screen');}
  catch(cause){
    console.error('Canvas click state',await page.evaluate(()=>({url:location.href,canvasCount:document.querySelectorAll('#screen').length,rect:document.querySelector('#screen')?.getBoundingClientRect().toJSON(),status:document.querySelector('#status')?.textContent,error:document.querySelector('#error')?.textContent})));
    await page.screenshot({path:'target/interactive/click-failure.png',timeout:5000}).catch(error=>console.error('Failure screenshot unavailable:',error.message));
    throw cause;
  }
  await pixel([0xe0,0x40,0x20]);
  await page.keyboard.press('a');await pixel([0x20,0xc0,0x60]);
  await page.keyboard.press('Escape');await status('Exited (0)');
  await consoleAction('#start');await pixel([0x12,0x34,0x56]);await status('waiting');
  await consoleAction('#stop');await status('Stopped');
  await consoleAction('#start');await status('waiting');await consoleAction('#close');await status('Exited (0)');
  // Substitute a valid guest module that loops forever, to exercise termination
  // without depending on the guest reaching an import or honoring a stop flag.
  const leb=value=>{const bytes=[];do{let b=value&127;value>>>=7;if(value)b|=128;bytes.push(b);}while(value);return bytes;};
  const section=(id,bytes)=>[id,...leb(bytes.length),...bytes];
  const name=text=>[...leb(text.length),...Buffer.from(text)];
  const spin=Buffer.from([0,97,115,109,1,0,0,0,
    ...section(1,[1,96,0,0]),...section(3,[1,0]),...section(5,[1,0,...leb(65536)]),
    ...section(7,[2,...name('entry'),0,0,...name('memory'),2,0]),...section(10,[1,7,0,3,64,12,0,11,11])]);
  await page.route('**/guest.wasm',route=>route.fulfill({body:spin,contentType:'application/wasm'}));
  await consoleAction('#start');await status('running');await consoleAction('#stop');await status('Stopped');
  await page.unroute('**/guest.wasm');await consoleAction('#start');await pixel([0x12,0x34,0x56]);await status('waiting');await consoleAction('#stop');
  await page.route('**/manifest.json',async route=>{const response=await route.fetch();const manifest=await response.json();manifest.imports.push({library:'TEST.dll',name:'MissingApi'});await route.fulfill({json:manifest});});
  await consoleAction('#start');await status('waiting');await consoleAction('#stop');
  // An unused missing import is allowed; actually calling it must name it.
  const missingBody=[0,0x41,0,0x41,0,0x10,0,0x1a,0x0b];
  const missing=Buffer.from([0,97,115,109,1,0,0,0,
    ...section(1,[2,96,0,0,96,2,127,127,1,127]),
    ...section(2,[1,...name('TEST.dll'),...name('MissingApi'),0,1]),
    ...section(3,[1,0]),...section(5,[1,0,...leb(65536)]),
    ...section(7,[2,...name('entry'),0,1,...name('memory'),2,0]),
    ...section(10,[1,...leb(missingBody.length),...missingBody])]);
  await page.route('**/guest.wasm',route=>route.fulfill({body:missing,contentType:'application/wasm'}));
  await consoleAction('#start');await status('Failed');assert.match(await page.textContent('#error'),/TEST.dll!MissingApi/);
  await page.unroute('**/manifest.json');await page.unroute('**/guest.wasm');
  // A real Wasm import blocks on a page dialog and checks the returned Cancel
  // result before returning. No original Hover assets are needed by this test.
  const dialogData=Buffer.alloc(0x310);
  dialogData.writeUInt32LE(0x30100,4);dialogData.writeUInt32LE(0x30200,8);dialogData.writeUInt32LE(1,12);
  dialogData.write('Guest asks a question',0x100);dialogData.write('Hover',0x200);
  const dialogBody=[0,0x41,...leb(0x30000),0x41,0,0x10,0,0x41,2,0x47,0x04,0x40,0,0x0b,
    0x41,...leb(0x30300),0x41,0,0x10,1,0x1a,0x0b];
  const modal=Buffer.from([0,97,115,109,1,0,0,0,
    ...section(1,[2,96,0,0,96,2,127,127,1,127]),
    ...section(2,[2,...name('USER32.dll'),...name('MessageBoxA'),0,1,...name('KERNEL32.dll'),...name('ExitProcess'),0,1]),
    ...section(3,[1,0]),...section(5,[1,0,...leb(65536)]),
    ...section(7,[2,...name('entry'),0,2,...name('memory'),2,0]),
    ...section(10,[1,...leb(dialogBody.length),...dialogBody]),
    ...section(11,[1,0,0x41,...leb(0x30000),0x0b,...leb(dialogData.length),...dialogData])]);
  await page.route('**/manifest.json',async route=>{const response=await route.fetch();const manifest=await response.json();manifest.imports=[{library:'USER32.dll',name:'MessageBoxA'},{library:'KERNEL32.dll',name:'ExitProcess'}];await route.fulfill({json:manifest});});
  await page.route('**/guest.wasm',route=>route.fulfill({body:modal,contentType:'application/wasm'}));
  await consoleAction('#start');await status('Waiting for dialog');
  assert.equal(await page.textContent('#dialog-title'),'Hover');
  assert.equal(await page.textContent('#dialog-text'),'Guest asks a question');
  await page.getByRole('button',{name:'Cancel',exact:true}).click();await status('Exited (0)');
  await consoleAction('#start');await status('Waiting for dialog');
  await page.keyboard.press('Escape');await status('Exited (0)');
  assert.deepEqual(errors,[]);
  console.log('Browser acceptance: pixels, input, waiting, close, Stop/Restart, runaway termination, named failure and modal dialog responses passed');
}finally{await browser.close();}
