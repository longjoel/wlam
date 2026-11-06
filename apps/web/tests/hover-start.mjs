// Observe the original Hover game surface after its first-run Quick Help.
import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';

const {chromium}=await import(process.env.WLAM_PLAYWRIGHT_MODULE??'playwright');
const browser=await chromium.launch({headless:true,args:['--no-sandbox']});
const page=await browser.newPage();
const errors=[];
page.on('pageerror',error=>errors.push(error.message));
try{
  await page.goto(process.env.WLAM_TEST_URL??'http://127.0.0.1:8765/apps/web/');
  await page.click('#desktop-launch');
  await page.locator('#manifest').fill('/target/hover/bundle/manifest.json');
  await page.locator('#start').click();
  const guest=page.locator('.guest-window');
  const screen=guest.locator('[data-role="screen"]');
  await page.waitForFunction(()=>document.querySelector('.guest-window')?.dataset.state==='waiting',null,{timeout:20000});
  await screen.focus();await page.keyboard.press('F2');
  await page.waitForFunction(()=>{
    const canvas=document.querySelector('.guest-window [data-role="screen"]');
    return canvas?.width===485&&canvas?.height===212;
  },null,{timeout:20000});
  const bounds=await screen.boundingBox();
  const quickHelpWindow=await screen.getAttribute('data-guest-window');
  console.log('Hover screen bounds:',JSON.stringify(bounds));
  await page.mouse.click(bounds.x+425*bounds.width/485,bounds.y+20*bounds.height/212);
  await page.waitForTimeout(800);
  const clickState=await screen.evaluate(canvas=>({width:canvas.width,height:canvas.height,guestWindow:canvas.dataset.guestWindow,lastPointer:canvas.dataset.lastPointer}));
  console.log('Hover after OK click:',JSON.stringify(clickState));
  assert.notDeepEqual([clickState.width,clickState.height],[485,212],'closing Quick Help should stop presenting its child surface');
  assert.notEqual(clickState.guestWindow,quickHelpWindow,'the destroyed dialog HWND must not remain the input target');
  await page.screenshot({path:'target/hover/after-ok-click.png'});
  await screen.focus();await page.keyboard.press('F2');await page.waitForTimeout(800);
  await page.waitForFunction(()=>document.querySelector('#guest-dialog').open,null,{timeout:10000});
  assert.match(await page.textContent('#dialog-text'),/quit the game in progress/i);
  const quitChoice=process.env.WLAM_HOVER_QUIT_CHOICE??'No';
  await page.getByRole('button',{name:quitChoice,exact:true}).click();
  await page.waitForFunction(()=>!document.querySelector('#guest-dialog').open,null,{timeout:10000});
  await page.waitForFunction(()=>document.querySelector('.guest-window')?.dataset.state==='waiting',null,{timeout:10000});
  await page.waitForTimeout(800);
  const before=await screen.evaluate(canvas=>{
    const pixels=canvas.getContext('2d').getImageData(0,0,canvas.width,canvas.height).data;
    let nonblack=0;const colors=new Set();
    for(let i=0;i<pixels.length;i+=4){
      const r=pixels[i],g=pixels[i+1],b=pixels[i+2];
      if(r||g||b)nonblack++;
      colors.add((r<<16)|(g<<8)|b);
    }
    return {width:canvas.width,height:canvas.height,nonblack,uniqueColors:colors.size};
  });
  await mkdir('target/hover',{recursive:true});
  await page.screenshot({path:'target/hover/after-quick-help.png'});
  if(quitChoice==='Yes'&&process.env.WLAM_HOVER_RESTART_AFTER_QUIT==='1'){
    await screen.focus();await page.keyboard.press('F2');await page.waitForTimeout(800);
    const promptOpen=await page.locator('#guest-dialog').evaluate(dialog=>dialog.open);
    const promptText=promptOpen?await page.textContent('#dialog-text'):'';
    if(promptOpen){
      const button=/quit|exit/i.test(promptText)?'No':'OK';
      await page.getByRole('button',{name:button,exact:true}).click();
      await page.waitForTimeout(500);
    }
    const restartState=await screen.evaluate(canvas=>({width:canvas.width,height:canvas.height,guestWindow:canvas.dataset.guestWindow}));
    console.log('Hover after F2 from quit state:',JSON.stringify({promptOpen,promptText,restartState,state:await guest.getAttribute('data-state'),error:await guest.locator('[data-role="error"]').textContent()}));
  }
  await screen.focus();await page.keyboard.down('ArrowUp');await page.waitForTimeout(500);await page.keyboard.up('ArrowUp');
  await page.waitForTimeout(800);
  const after=await screen.evaluate(canvas=>{
    const pixels=canvas.getContext('2d').getImageData(0,0,canvas.width,canvas.height).data;
    let nonblack=0;const colors=new Set();
    for(let i=0;i<pixels.length;i+=4){
      const r=pixels[i],g=pixels[i+1],b=pixels[i+2];
      if(r||g||b)nonblack++;
      colors.add((r<<16)|(g<<8)|b);
    }
    return {width:canvas.width,height:canvas.height,nonblack,uniqueColors:colors.size};
  });
  await page.screenshot({path:'target/hover/after-arrow-up.png'});
  if(process.env.WLAM_HOVER_TOGGLE_PAUSE==='1'){
    const beforePause=JSON.parse(await page.locator('#debug-report').textContent());
    await screen.focus();await page.keyboard.press('F3');await page.waitForTimeout(500);
    const pauseReport=JSON.parse(await page.locator('#debug-report').textContent());
    const pauseGdiCalls=(pauseReport.recentApis??[]).filter(api=>api.sequence>beforePause.lastEventSequence&&api.library?.toLowerCase().startsWith('gdi32'));
    const pixels=await screen.evaluate(canvas=>{
      const data=canvas.getContext('2d').getImageData(0,0,canvas.width,canvas.height).data;
      let nonblack=0;for(let i=0;i<data.length;i+=4)if(data[i]||data[i+1]||data[i+2])nonblack++;
      return {width:canvas.width,height:canvas.height,nonblack};
    });
    await writeFile('target/hover/f3-api-trace.json',JSON.stringify(pauseReport,null,2)+'\n');
    console.log('Hover after F3:',JSON.stringify({pixels,gdiCalls:pauseGdiCalls,callbacks:(pauseReport.recentCallbacks??[]).filter(callback=>callback.sequence>beforePause.lastEventSequence)}));
  }
  const result={before,after,state:await guest.getAttribute('data-state'),error:await guest.locator('[data-role="error"]').textContent(),pageErrors:errors};
  console.log('Hover after Quick Help:',JSON.stringify(result));
  const debugReport=JSON.parse(await page.locator('#debug-report').textContent());
  const arrowCallbacks=(debugReport.recentCallbacks??[]).filter(callback=>callback.args?.[1]===0x100&&callback.args?.[2]===38);
  const arrowSequence=Math.max(0,...arrowCallbacks.map(callback=>callback.sequence??0));
  const gdiCallsAfterArrow=(debugReport.recentApis??[]).filter(api=>api.sequence>arrowSequence&&api.library?.toLowerCase().startsWith('gdi32'));
  await writeFile('target/hover/arrow-up-api-trace.json',JSON.stringify(debugReport,null,2)+'\n');
  console.log('ArrowUp callbacks:',JSON.stringify(arrowCallbacks));
  console.log('GDI calls after ArrowUp:',JSON.stringify(gdiCallsAfterArrow));
  assert.equal(result.state,'waiting');
  assert.equal(result.error,'');
  assert.deepEqual(errors,[]);
  assert.ok(arrowCallbacks.length,'ArrowUp must reach the guest window procedure');
  await guest.locator('[data-role="window-close"]').click();
}finally{await browser.close();}
