import {BUFFER_BYTES,enqueue,RESIZE_GUEST_WINDOW} from './queue.mjs';
import {dialogButtons,DIALOG_RESPONSE} from './dialog.mjs';
import {WebGlRenderer} from './webgl-renderer.mjs';
import {formatDebugReport} from './diagnostics.mjs';
const dialog=document.querySelector('#guest-dialog');
const screen=document.querySelector('#screen'),status=document.querySelector('#status'),error=document.querySelector('#error');
const appWindow=document.querySelector('#app-window'),titlebar=document.querySelector('#titlebar');
const statusDot=document.querySelector('#status-dot');
new MutationObserver(()=>statusDot.classList.toggle('live',status.textContent==='running')).observe(status,{childList:true,characterData:true,subtree:true});
const stopButton=document.querySelector('#stop'),closeButton=document.querySelector('#close');
const resizeButton=document.querySelector('#resize-guest');
const resizeWidth=document.querySelector('#guest-width'),resizeHeight=document.querySelector('#guest-height');
const debugPanel=document.querySelector('#debugger'),downloadDebug=document.querySelector('#download-debug');
let debugReport=null;
const hex=value=>value===null?'unavailable':`0x${(value>>>0).toString(16).padStart(8,'0')}`;
function updateDebug(report){
  debugReport={...report,runner:{url:location.href,userAgent:navigator.userAgent,crossOriginIsolated,manifest:document.querySelector('#manifest').value}};
  downloadDebug.disabled=false;
  const api=report.failedApi;
  document.querySelector('#debug-summary').textContent=[
    report.kind==='failure'?(report.reason??'Guest execution failed'):`Last snapshot: ${report.kind}`,
    api?`Failed API: ${api.library}!${api.name}(${api.args.map(hex).join(', ')})${api.argumentCountKnown===false?' [raw stack words; argument count unknown]':''}`:'',
    api?`Guest return address: ${hex(api.returnAddress)}`:'',
    report.guest?Object.entries(report.guest.registers).map(([name,value])=>`${name}=${hex(value)}`).join(' '):'',
    `${report.recentApis?.length??0} recent API calls captured.`,
  ].filter(Boolean).join('\n');
  document.querySelector('#debug-report').textContent=formatDebugReport(debugReport);
  if(report.kind==='failure')debugPanel.open=true;
}
downloadDebug.onclick=()=>{
  if(!debugReport)return;
  const url=URL.createObjectURL(new Blob([formatDebugReport(debugReport)],{type:'application/json'}));
  const link=document.createElement('a');link.href=url;link.download=`wlam-debug-${new Date().toISOString().replace(/[:.]/g,'-')}.json`;
  document.body.append(link);link.click();link.remove();setTimeout(()=>URL.revokeObjectURL(url),1000);
};
const manifest=new URLSearchParams(location.search).get('manifest');
if(manifest)document.querySelector('#manifest').value=manifest;
const webglRequested=new URLSearchParams(location.search).get('renderer')==='webgl';
let webglRenderer=null,webglInitError='';
if(webglRequested){try{webglRenderer=new WebGlRenderer(screen);}catch(cause){webglInitError=cause.message;}}
let worker,input,window=0,generation=0,resizeDirty=false,pendingResize=null,resizeTimer=0,queuedGuestSize=null,desiredGuestSize=null;
function stop(){generation++;worker?.terminate();worker=null;window=0;pendingResize=null;queuedGuestSize=null;clearTimeout(resizeTimer);resizeDirty=false;dialog.close();stopButton.disabled=true;closeButton.disabled=true;resizeButton.disabled=true;status.textContent='Stopped';}
function markWindowState(state){appWindow.classList.toggle('is-minimized',state==='minimized');document.querySelector('#task-app').classList.toggle('active',state!=='minimized');if(state!=='minimized')appWindow.classList.remove('is-minimized');}
function fitAppWindowToDesktop(){
  if(appWindow.classList.contains('is-maximized')||appWindow.classList.contains('is-minimized'))return;
  const rect=appWindow.getBoundingClientRect();
  const maxWidth=Math.max(1,innerWidth-16),maxHeight=Math.max(230,innerHeight-48-16);
  const width=Math.min(rect.width,maxWidth),height=Math.min(rect.height,maxHeight);
  const left=Math.max(8,Math.min(rect.left,innerWidth-width-8));
  const top=Math.max(8,Math.min(rect.top,innerHeight-48-height-8));
  appWindow.style.left=`${left}px`;appWindow.style.top=`${top}px`;
  appWindow.style.width=`${width}px`;appWindow.style.height=`${height}px`;
  appWindow.style.transform='none';
}
function queueGuestResize(width,height){
  const nextWidth=Math.max(1,Math.min(4096,Math.round(width))),nextHeight=Math.max(1,Math.min(4096,Math.round(height)));
  resizeWidth.value=nextWidth;resizeHeight.value=nextHeight;resizeDirty=true;desiredGuestSize=[nextWidth,nextHeight];
  if(!resizeButton.disabled){queuedGuestSize=[nextWidth,nextHeight];clearTimeout(resizeTimer);resizeTimer=setTimeout(flushGuestResize,90);}
}
function flushGuestResize(){if(!queuedGuestSize||resizeButton.disabled)return;const [width,height]=queuedGuestSize;queuedGuestSize=null;error.textContent='';send(RESIZE_GUEST_WINDOW,width,height);pendingResize=[width,height];}
function resizeGuestToViewport(){const viewport=document.querySelector('#screen-wrap');if(viewport.clientWidth&&viewport.clientHeight)queueGuestResize(viewport.clientWidth-24,viewport.clientHeight-24);}
function send(message,wparam=0,lparam=0){if(!window)return;try{enqueue(input,{window,message,wparam,lparam});}catch(cause){error.textContent=cause.message;stop();}}
document.querySelector('#start').onclick=()=>{
  stop();error.textContent='';queuedGuestSize=desiredGuestSize?[...desiredGuestSize]:null;resizeDirty=Boolean(queuedGuestSize);
  updateDebug({version:1,time:new Date().toISOString(),kind:'loading',recentApis:[]});
  if(webglRequested&&!webglRenderer){error.textContent=webglInitError||'WebGL renderer is unavailable.';status.textContent='Failed';return;}
  pendingResize=null;clearTimeout(resizeTimer);
  if(!crossOriginIsolated || typeof SharedArrayBuffer==='undefined'){error.textContent='Shared memory requires the local server with cross-origin isolation headers.';return;}
  input=new SharedArrayBuffer(BUFFER_BYTES);const gpuPacing=webglRequested?new SharedArrayBuffer(4):null;worker=new Worker(new URL('./worker.mjs',import.meta.url),{type:'module'});
  const current=generation;stopButton.disabled=false;status.textContent='Loading';
  worker.onmessage=({data})=>{
    if(current!==generation)return;
    if(data.type==='diagnostic')updateDebug(data.report);
    if(data.type==='dialog'){
      status.textContent='Waiting for dialog';
      document.querySelector('#dialog-title').textContent=data.title;
      document.querySelector('#dialog-text').textContent=data.text;
      const buttons=document.querySelector('#dialog-buttons');buttons.replaceChildren();
      const choices=dialogButtons(data.flags);
      const answer=id=>{if(current!==generation)return;enqueue(input,{window:0,message:DIALOG_RESPONSE,wparam:id});dialog.close();status.textContent='running';};
      for(const [id,label] of choices){const button=document.createElement('button');button.textContent=label;button.onclick=()=>answer(id);buttons.append(button);}
      dialog.oncancel=event=>{event.preventDefault();if(choices.some(([id])=>id===2))answer(2);else if(choices.length===1)answer(1);};
      dialog.showModal();buttons.children[Math.min((data.flags>>8)&3,choices.length-1)].focus();
    }
    if(data.type==='gpu-frame'){
      try{
        webglRenderer.render(new Uint32Array(data.words));window=data.window;
        screen.dataset.webglFrames=String(Number(screen.dataset.webglFrames||0)+1);
        closeButton.disabled=false;resizeButton.disabled=false;
        if(pendingResize&&pendingResize[0]===webglRenderer.canvas.width&&pendingResize[1]===webglRenderer.canvas.height){pendingResize=null;resizeDirty=false;}
        if(!resizeDirty){resizeWidth.value=webglRenderer.canvas.width;resizeHeight.value=webglRenderer.canvas.height;}
        flushGuestResize();
        requestAnimationFrame(()=>{if(current===generation&&gpuPacing){Atomics.store(new Int32Array(gpuPacing),0,data.sequence+1);Atomics.notify(new Int32Array(gpuPacing),0);}});
      }catch(cause){error.textContent=cause.stack??cause.message;status.textContent='Failed';generation++;worker?.terminate();worker=null;stopButton.disabled=true;closeButton.disabled=true;resizeButton.disabled=true;}
    }
    if(data.type==='frame'&&!webglRequested){window=data.window;screen.width=data.width;screen.height=data.height;screen.getContext('2d').putImageData(new ImageData(data.pixels,data.width,data.height),0,0);closeButton.disabled=false;resizeButton.disabled=false;if(pendingResize&&pendingResize[0]===data.width&&pendingResize[1]===data.height){pendingResize=null;resizeDirty=false;}if(!resizeDirty){resizeWidth.value=data.width;resizeHeight.value=data.height;}flushGuestResize();}
    if(data.type==='status'){status.textContent=data.state==='exited'?`Exited (${data.code})`:data.state;if(data.state==='exited'){stopButton.disabled=true;closeButton.disabled=true;window=0;}}
    if(data.type==='error'){
      error.textContent=debugReport?.reason??data.message.split('\n')[0];status.textContent='Failed';closeButton.disabled=true;
      updateDebug({...debugReport,kind:'failure',reason:debugReport?.reason??data.message.split('\n')[0],runtimeError:data.message,javascriptStack:data.stack});
    }
  };
  worker.onerror=event=>{
    error.textContent=event.message;status.textContent='Failed';
    updateDebug({...debugReport,kind:'failure',reason:event.message,workerError:{file:event.filename,line:event.lineno,column:event.colno}});
  };
  const hostProfile=webglRequested?'release':'debug';
  worker.postMessage({manifestURL:new URL(document.querySelector('#manifest').value,location.href).href,hostURL:new URL(`/target/wasm32-unknown-unknown/${hostProfile}/wlam_browser_host.wasm`,location.href).href,input,webgl:webglRequested,gpuPacing});
};
stopButton.onclick=stop;closeButton.onclick=()=>send(0x10);
resizeButton.onclick=()=>{
  const width=Number(resizeWidth.value),height=Number(resizeHeight.value);
  if(!Number.isInteger(width)||!Number.isInteger(height)||width<1||height<1||width>4096||height>4096){error.textContent='Guest dimensions must be between 1 and 4096 pixels.';return;}
  error.textContent='';desiredGuestSize=[width,height];queuedGuestSize=null;send(RESIZE_GUEST_WINDOW,width,height);
  pendingResize=[width,height];resizeDirty=true;
};
resizeWidth.oninput=resizeHeight.oninput=()=>{resizeDirty=true;};
screen.onpointerdown=event=>{screen.focus();screen.setPointerCapture(event.pointerId);const r=screen.getBoundingClientRect();const x=Math.floor((event.clientX-r.left)*screen.width/r.width),y=Math.floor((event.clientY-r.top)*screen.height/r.height);send(0x201,1,(x&65535)|((y&65535)<<16));};
const startMenu=document.querySelector('#start-menu'),startMenuButton=document.querySelector('#start-menu-button');
function toggleMenu(open=!startMenu.classList.contains('open')){startMenu.classList.toggle('open',open);startMenuButton.setAttribute('aria-expanded',String(open));}
startMenuButton.onclick=()=>toggleMenu();
function restoreRunner(){markWindowState('normal');toggleMenu(false);requestAnimationFrame(()=>{fitAppWindowToDesktop();resizeGuestToViewport();});}
document.querySelector('#menu-runner').onclick=document.querySelector('#desktop-launch').onclick=document.querySelector('#task-app').onclick=restoreRunner;
document.querySelector('#menu-restart').onclick=()=>{markWindowState('normal');toggleMenu(false);document.querySelector('#start').click();};
document.querySelector('#window-minimize').onclick=()=>markWindowState('minimized');
document.querySelector('#window-maximize').onclick=()=>{const maximized=appWindow.classList.toggle('is-maximized');document.querySelector('#window-maximize').textContent=maximized?'❐':'□';document.querySelector('#window-maximize').setAttribute('aria-label',maximized?'Restore window':'Maximize window');requestAnimationFrame(resizeGuestToViewport);};
document.querySelector('#window-close').onclick=()=>{if(!closeButton.disabled)closeButton.click();markWindowState('minimized');};
document.addEventListener('pointerdown',event=>{if(!startMenu.contains(event.target)&&event.target!==startMenuButton)toggleMenu(false);});
let drag=null;
titlebar.onpointerdown=event=>{if(event.button!==0||event.target.closest('.window-controls')||appWindow.classList.contains('is-maximized'))return;const rect=appWindow.getBoundingClientRect();drag={x:event.clientX,y:event.clientY,left:rect.left,top:rect.top};titlebar.setPointerCapture(event.pointerId);};
titlebar.onpointermove=event=>{if(!drag)return;const dx=event.clientX-drag.x,dy=event.clientY-drag.y;appWindow.style.left=`${Math.max(-appWindow.offsetWidth+100,Math.min(innerWidth-100,drag.left+dx))}px`;appWindow.style.top=`${Math.max(0,Math.min(innerHeight-80,drag.top+dy))}px`;appWindow.style.transform='none';};
titlebar.onpointerup=titlebar.onpointercancel=()=>{drag=null;};
titlebar.ondblclick=event=>{if(event.target.closest('.window-controls'))return;document.querySelector('#window-maximize').click();};
let resizing=false;
const resizeGrip=document.querySelector('#resize-grip');
function setAppWindowSize(width,height){
  const rect=appWindow.getBoundingClientRect();
  const minWidth=Math.min(320,innerWidth-16),minHeight=Math.min(230,innerHeight-16);
  const maxWidth=Math.max(minWidth,innerWidth-rect.left-8),maxHeight=Math.max(minHeight,innerHeight-rect.top-56);
  appWindow.style.width=`${Math.round(Math.max(minWidth,Math.min(maxWidth,width)))}px`;
  appWindow.style.height=`${Math.round(Math.max(minHeight,Math.min(maxHeight,height)))}px`;
  resizeGuestToViewport();
}
resizeGrip.onpointerdown=event=>{if(event.button!==0||appWindow.classList.contains('is-maximized'))return;resizing=true;resizeGrip.setPointerCapture(event.pointerId);event.preventDefault();};
resizeGrip.onpointermove=event=>{if(!resizing)return;const r=appWindow.getBoundingClientRect();setAppWindowSize(event.clientX-r.left,event.clientY-r.top);};
resizeGrip.onpointerup=resizeGrip.onpointercancel=()=>{resizing=false;clearTimeout(resizeTimer);flushGuestResize();};
resizeGrip.onkeydown=event=>{
  const amount=event.shiftKey?50:10;
  const delta={ArrowLeft:[-amount,0],ArrowRight:[amount,0],ArrowUp:[0,-amount],ArrowDown:[0,amount],Enter:[amount,amount],' ':[amount,amount]}[event.key];
  if(!delta||appWindow.classList.contains('is-maximized'))return;
  event.preventDefault();setAppWindowSize(appWindow.offsetWidth+delta[0],appWindow.offsetHeight+delta[1]);
};
globalThis.addEventListener('resize',()=>requestAnimationFrame(()=>{
  if(appWindow.classList.contains('is-minimized'))return;
  fitAppWindowToDesktop();resizeGuestToViewport();
}));
const clock=document.querySelector('#task-clock');
const updateClock=()=>clock.textContent=new Intl.DateTimeFormat(undefined,{hour:'numeric',minute:'2-digit'}).format(new Date());
updateClock();setInterval(updateClock,15000);
const keys={Escape:27,Enter:13,ArrowLeft:37,ArrowUp:38,ArrowRight:39,ArrowDown:40,Shift:16,Control:17,Alt:18,' ':32};
function keycode(event){return keys[event.key]??(/^F([1-9]|1[0-2])$/.test(event.key)?111+Number(event.key.slice(1)):/^[a-z0-9]$/i.test(event.key)?event.key.toUpperCase().charCodeAt(0):null);}
screen.onkeydown=event=>{const key=keycode(event);if(key!==null){event.preventDefault();send(0x100,key);}};
screen.onkeyup=event=>{const key=keycode(event);if(key!==null){event.preventDefault();send(0x101,key);}};
