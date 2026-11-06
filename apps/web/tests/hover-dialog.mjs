// Optional acceptance check requiring the user's original Hover bundle.
import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
const {chromium}=await import(process.env.WLAM_PLAYWRIGHT_MODULE??'playwright');
const browser=await chromium.launch({headless:true,args:['--no-sandbox']});
const page=await browser.newPage();const errors=[];
page.on('pageerror',error=>errors.push(error.message));
try{
  await page.goto(process.env.WLAM_TEST_URL??'http://127.0.0.1:8765/apps/web/');
  await page.waitForSelector('[data-bundle="hover"]');
  await page.click('[data-bundle="hover"]');
  const guest=page.locator('.guest-window');
  const screen=guest.locator('[data-role="screen"]');
  await page.waitForFunction(()=>document.querySelector('.guest-window')?.dataset.state==='waiting',null,{timeout:20000});
  await screen.focus();await page.keyboard.press('F2');
  await page.waitForFunction(()=>{const guest=document.querySelector('.guest-window'),canvas=guest?.querySelector('[data-role="screen"]');return canvas?.width===485&&canvas?.height===212&&guest.dataset.state==='waiting';},null,{timeout:20000});
  const pixel=async color=>page.waitForFunction(expected=>{
    const canvas=document.querySelector('#screen');const p=canvas.getContext('2d').getImageData(12,195,1,1).data;
    return p[0]===expected&&p[1]===expected&&p[2]===expected;
  },color);
  await pixel(255);
  const darkPixels=await screen.evaluate(canvas=>{
    const pixels=canvas.getContext('2d').getImageData(20,25,330,32).data;let dark=0;
    for(let i=0;i<pixels.length;i+=4)if(pixels[i]<100&&pixels[i+1]<100&&pixels[i+2]<100)dark++;
    return dark;
  });
  assert.ok(darkPixels>20,`Quick Help instructions should be painted; found ${darkPixels} dark pixels in the instruction region`);
  const bounds=await screen.boundingBox();
  await page.mouse.click(bounds.x+12*bounds.width/485,bounds.y+195*bounds.height/212);
  await pixel(0);
  await page.keyboard.press('Space');await pixel(255);
  assert.equal(await guest.locator('[data-role="error"]').textContent(),'');assert.deepEqual(errors,[]);
  if(process.env.WLAM_HOVER_CONTINUE==='1'){
    const bounds=await screen.boundingBox();
    await page.mouse.click(bounds.x+425*bounds.width/485,bounds.y+20*bounds.height/212);
    await page.waitForTimeout(1000);await screen.focus();await page.keyboard.press('F2');
    await page.waitForFunction(()=>document.querySelector('#guest-dialog').open,null,{timeout:10000});
    assert.equal(await page.textContent('#dialog-title'),'Hover!');
    assert.match(await page.textContent('#dialog-text'),/quit the game in progress/i);
    await page.getByRole('button',{name:'No',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('#guest-dialog').open===false);
    await page.waitForTimeout(1000);
    const after=await screen.evaluate(canvas=>({width:canvas.width,height:canvas.height,state:canvas.closest('.guest-window').dataset.state,error:canvas.closest('.guest-window').querySelector('[data-role="error"]').textContent}));
    console.log('Hover after Quick Help:',JSON.stringify(after));
    assert.notEqual(after.state,'failed',after.error||'Hover failed after dismissing Quick Help');
    await page.screenshot({path:'target/hover/after-preferences.png'});
  }
  await mkdir('target/hover',{recursive:true});await page.screenshot({path:'target/hover/quick-help.png'});
  await guest.locator('[data-role="window-close"]').click();
  console.log('Hover Quick Help: child painting, pointer and keyboard checkbox toggling passed; gameplay is not covered');
}finally{await browser.close();}
