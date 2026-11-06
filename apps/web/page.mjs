import {BUFFER_BYTES,enqueue,RESIZE_GUEST_WINDOW} from './queue.mjs';
import {dialogButtons,DIALOG_RESPONSE} from './dialog.mjs';
import {WebGlRenderer} from './webgl-renderer.mjs';
import {formatDebugReport} from './diagnostics.mjs';

const dialog=document.querySelector('#guest-dialog');
const taskbar=document.querySelector('#taskbar');
const windowLayer=document.querySelector('#window-layer');
const desktopIcons=document.querySelector('#desktop-icons');
const dynamicMenu=document.querySelector('#window-menu-items');
const startMenu=document.querySelector('#start-menu'),startMenuButton=document.querySelector('#start-menu-button');
const guestTemplate=document.querySelector('#guest-window-template');
const RECOMPILE_API='/apps/web/recompile';
const dropOverlay=document.querySelector('#drop-overlay');
const toastElement=document.querySelector('#desktop-toast');
const urlParameters=new URLSearchParams(location.search);
const urlManifest=urlParameters.get('manifest');
const urlRenderer=urlParameters.get('renderer');
const DEFAULT_MANIFEST='/target/interactive/bundle/manifest.json';
const windows=[];
let zIndex=10;
const hex=value=>value===null?'unavailable':`0x${(value>>>0).toString(16).padStart(8,'0')}`;
const keys={Escape:27,Enter:13,ArrowLeft:37,ArrowUp:38,ArrowRight:39,ArrowDown:40,Shift:16,Control:17,Alt:18,' ':32};
function keycode(event){return keys[event.key]??(/^F([1-9]|1[0-2])$/.test(event.key)?111+Number(event.key.slice(1)):/^[a-z0-9]$/i.test(event.key)?event.key.toUpperCase().charCodeAt(0):null);}
function titleFromManifest(manifest){
  const match=/target\/([^/]+)\/bundle\/manifest\.json$/.exec(manifest);
  if(!match)return 'Guest';
  return match[1].replace(/[-_]+/g,' ').replace(/\b\w/g,letter=>letter.toUpperCase());
}

function uiNode(tag,className,text){
  const node=document.createElement(tag);
  if(className)node.className=className;
  if(text!==undefined)node.textContent=text;
  return node;
}
let toastTimer=0;
function toast(text,duration=6000){
  toastElement.textContent=text;toastElement.hidden=false;
  clearTimeout(toastTimer);toastTimer=setTimeout(()=>{toastElement.hidden=true;},duration);
}
function formatBytes(value){
  if(!Number.isFinite(value))return '—';
  const units=['B','KiB','MiB','GiB'];
  let index=0,amount=value;
  while(amount>=1024&&index<units.length-1){amount/=1024;index++;}
  return `${amount.toFixed(index?1:0)} ${units[index]}`;
}
async function apiRequest(url,options){
  const response=await fetch(url,options);
  let data={};
  try{data=await response.json();}catch{}
  if(!response.ok)throw Error(data.error??`Request failed (${response.status})`);
  return data;
}

// The desktop may run several guests at once, so message boxes are queued
// instead of letting two workers call showModal on the same dialog element.
let dialogOwner=null;
const dialogQueue=[];
function closeDialogsFor(owner){
  for(let index=dialogQueue.length-1;index>=0;index--)if(dialogQueue[index].owner===owner)dialogQueue.splice(index,1);
  if(dialogOwner!==owner)return;
  dialogOwner=null;
  if(dialog.open)dialog.close();
  pumpDialog();
}
function showGuestDialog(owner,data,answer){dialogQueue.push({owner,data,answer});pumpDialog();}
function pumpDialog(){
  if(dialogOwner||!dialogQueue.length)return;
  const {owner,data,answer}=dialogQueue.shift();
  dialogOwner=owner;
  document.querySelector('#dialog-title').textContent=data.title;
  document.querySelector('#dialog-text').textContent=data.text;
  const choices=dialogButtons(data.flags)??[];
  const buttons=document.querySelector('#dialog-buttons');buttons.replaceChildren();
  const finish=id=>{
    if(dialogOwner!==owner)return;
    dialogOwner=null;
    if(dialog.open)dialog.close();
    answer(id);
    pumpDialog();
  };
  for(const [id,label] of choices){const button=document.createElement('button');button.textContent=label;button.onclick=()=>finish(id);buttons.append(button);}
  dialog.oncancel=event=>{
    event.preventDefault();
    if(choices.some(([id])=>id===2))finish(2);
    else if(choices.length===1)finish(choices[0][0]);
  };
  dialog.showModal();
  buttons.children[Math.min((data.flags>>8)&3,Math.max(0,choices.length-1))]?.focus();
}

function createTaskButton(title,icon){
  const button=document.createElement('button');
  button.className='task-app';
  const glyph=document.createElement('span');glyph.setAttribute('aria-hidden','true');glyph.textContent=icon;
  const label=document.createElement('span');label.className='task-label';label.textContent=title;
  button.append(glyph,label);
  taskbar.insertBefore(button,taskbar.querySelector('.task-spacer'));
  return button;
}
function createMenuButton(title,icon){
  const button=document.createElement('button');
  button.className='menu-item';
  const glyph=document.createElement('span');glyph.className='glyph';glyph.setAttribute('aria-hidden','true');glyph.textContent=icon;
  const label=document.createElement('span');label.textContent=title;
  button.append(glyph,label);
  dynamicMenu.append(button);
  return button;
}

// Shared simulated-window behaviour: focus order, title-bar drag, minimize and
// maximize. Guest windows and the developer console both sit on this.
class DesktopWindow{
  constructor({root,title,icon,taskButton,restoreItem,restartItem}){
    this.root=root;this.title=title;
    this.taskButton=taskButton;this.restoreItem=restoreItem;this.restartItem=restartItem;
    const role=name=>root.querySelector(`[data-role="${name}"]`);
    this.titlebar=role('titlebar');this.titleIcon=role('title-icon');this.titleText=role('title-text');
    this.resizeGrip=role('resize-grip');
    this.taskGlyph=taskButton.querySelector('[aria-hidden="true"]');
    this.taskLabel=taskButton.querySelector('.task-label')??taskButton.querySelector('#task-label');
    this.titleText.textContent=title;this.titleIcon.textContent=icon;
    this.taskGlyph.textContent=icon;this.taskLabel.textContent=title;
    this.taskButton.onclick=()=>this.reveal();
    this.restoreItem.onclick=()=>this.reveal();
    this.restartItem.onclick=()=>{this.reveal();this.restart?.();};
    this.root.addEventListener('pointerdown',()=>this.focusWindow());
    this.bindChrome();
    if(!windows.includes(this))windows.push(this);
  }
  focusWindow(){this.root.style.zIndex=String(++zIndex);}
  markWindowState(state){
    this.root.classList.toggle('is-minimized',state==='minimized');
    this.taskButton.classList.toggle('active',state!=='minimized');
    if(state!=='minimized')this.root.classList.remove('is-minimized');
  }
  reveal(){
    this.root.hidden=false;this.taskButton.hidden=false;
    this.markWindowState('normal');toggleMenu(false);this.focusWindow();
    requestAnimationFrame(()=>{this.fitToDesktop();this.resizeGuestToViewport?.();});
  }
  positionCascade(){
    const index=Math.max(0,windows.indexOf(this));
    const width=Math.min(880,innerWidth-36),height=Math.min(650,innerHeight-48-36);
    const offset=(index%6)-2;
    const left=Math.max(8,Math.round((innerWidth-width)/2)+offset*26);
    const top=Math.max(8,Math.round((innerHeight-48-height)/2)+offset*20);
    Object.assign(this.root.style,{left:`${left}px`,top:`${top}px`,width:`${width}px`,height:`${height}px`,transform:'none'});
  }
  fitToDesktop(){
    if(this.root.hidden||this.root.classList.contains('is-maximized')||this.root.classList.contains('is-minimized'))return;
    const rect=this.root.getBoundingClientRect();
    const maxWidth=Math.max(1,innerWidth-16),maxHeight=Math.max(230,innerHeight-48-16);
    const width=Math.min(rect.width,maxWidth),height=Math.min(rect.height,maxHeight);
    const left=Math.max(8,Math.min(rect.left,innerWidth-width-8));
    const top=Math.max(8,Math.min(rect.top,innerHeight-48-height-8));
    this.root.style.left=`${left}px`;this.root.style.top=`${top}px`;
    this.root.style.width=`${width}px`;this.root.style.height=`${height}px`;
    this.root.style.transform='none';
  }
  setWindowSize(width,height){
    const rect=this.root.getBoundingClientRect();
    const minWidth=Math.min(320,innerWidth-16),minHeight=Math.min(230,innerHeight-16);
    const maxWidth=Math.max(minWidth,innerWidth-rect.left-8),maxHeight=Math.max(minHeight,innerHeight-rect.top-56);
    this.root.style.width=`${Math.round(Math.max(minWidth,Math.min(maxWidth,width)))}px`;
    this.root.style.height=`${Math.round(Math.max(minHeight,Math.min(maxHeight,height)))}px`;
    this.resizeGuestToViewport?.();
  }
  removeUI(){
    this.root.remove();this.taskButton.remove();this.restoreItem.remove();this.restartItem.remove();
    const index=windows.indexOf(this);
    if(index>=0)windows.splice(index,1);
  }
  close(){this.removeUI();}
  bindChrome(){
    const button=name=>this.root.querySelector(`[data-role="${name}"]`);
    button('window-minimize').onclick=()=>{
      this.markWindowState('minimized');
      this.onWindowMinimize?.();
    };
    const maximizeButton=button('window-maximize');
    maximizeButton.onclick=()=>{
      const maximized=this.root.classList.toggle('is-maximized');
      maximizeButton.textContent=maximized?'❐':'□';
      maximizeButton.setAttribute('aria-label',maximized?'Restore window':'Maximize window');
      this.onWindowMaximize?.(maximized);
      requestAnimationFrame(()=>this.resizeGuestToViewport?.());
    };
    button('window-close').onclick=()=>this.close();
    let drag=null;
    this.titlebar.onpointerdown=event=>{
      if(event.button!==0||event.target.closest('.window-controls')||this.root.classList.contains('is-maximized'))return;
      const rect=this.root.getBoundingClientRect();drag={x:event.clientX,y:event.clientY,left:rect.left,top:rect.top};
      this.titlebar.setPointerCapture(event.pointerId);
    };
    this.titlebar.onpointermove=event=>{
      if(!drag)return;
      const dx=event.clientX-drag.x,dy=event.clientY-drag.y;
      this.root.style.left=`${Math.max(-this.root.offsetWidth+100,Math.min(innerWidth-100,drag.left+dx))}px`;
      this.root.style.top=`${Math.max(0,Math.min(innerHeight-80,drag.top+dy))}px`;
      this.root.style.transform='none';
    };
    this.titlebar.onpointerup=this.titlebar.onpointercancel=()=>{drag=null;};
    this.titlebar.ondblclick=event=>{if(event.target.closest('.window-controls'))return;maximizeButton.click();};
    if(!this.resizeGrip)return;
    let resizing=false;
    this.resizeGrip.onpointerdown=event=>{
      if(event.button!==0||this.root.classList.contains('is-maximized'))return;
      resizing=true;this.resizeGrip.setPointerCapture(event.pointerId);event.preventDefault();
    };
    this.resizeGrip.onpointermove=event=>{
      if(!resizing)return;
      const rect=this.root.getBoundingClientRect();
      this.setWindowSize(event.clientX-rect.left,event.clientY-rect.top);
    };
    this.resizeGrip.onpointerup=this.resizeGrip.onpointercancel=()=>{resizing=false;this.flushGuestResize?.();};
    this.resizeGrip.onkeydown=event=>{
      const amount=event.shiftKey?50:10;
      const delta={ArrowLeft:[-amount,0],ArrowRight:[amount,0],ArrowUp:[0,-amount],ArrowDown:[0,amount],Enter:[amount,amount],' ':[amount,amount]}[event.key];
      if(!delta||this.root.classList.contains('is-maximized'))return;
      event.preventDefault();this.setWindowSize(this.root.offsetWidth+delta[0],this.root.offsetHeight+delta[1]);
    };
  }
}

// A bundle window is only a title bar and the guest's pixels. The guest
// surface always matches the window client area; there are no size fields.
class GuestWindow extends DesktopWindow{
  constructor({title,icon='🖥️',manifest,renderer=null,onStatus,onDebug,onSize,onClosed,onError}){
    const root=guestTemplate.content.firstElementChild.cloneNode(true);
    root.setAttribute('aria-label',`${title} window`);
    windowLayer.append(root);
    const taskButton=createTaskButton(title,icon);
    const restoreItem=createMenuButton(title,icon);
    const restartItem=createMenuButton(`Restart ${title}`,'↻');
    super({root,title,icon,taskButton,restoreItem,restartItem});
    this.manifestURLValue=new URL(manifest,location.href).href;
    this.renderer=renderer;
    this.onStatus=onStatus;this.onDebug=onDebug;this.onSize=onSize;this.onClosed=onClosed;this.onError=onError;
    this.restart=()=>this.start();
    const role=name=>root.querySelector(`[data-role="${name}"]`);
    this.status=role('status');this.statusDot=role('status-dot');this.error=role('error');
    this.screen=role('screen');this.screenWrap=role('screen-wrap');
    this.webglRequested=renderer==='webgl';
    this.webglRenderer=null;this.webglInitError='';
    if(this.webglRequested){try{this.webglRenderer=new WebGlRenderer(this.screen);}catch(cause){this.webglInitError=cause.message;}}
    this.worker=null;this.input=null;this.guestWindow=0;this.generation=0;
    this.resizeDirty=false;this.pendingResize=null;this.resizeTimer=0;this.queuedGuestSize=null;this.desiredGuestSize=null;
    this.debugReport=null;this.state='ready';
    this.claimScreenId();
    this.bindGuestInput();
  }
  // Keep a single active #screen for pages and tests that address the focused
  // guest directly, matching a real desktop's one-focused-window model.
  claimScreenId(){
    for(const window of windows)if(window!==this&&window.screen instanceof HTMLCanvasElement)window.screen.removeAttribute('id');
    this.screen.id='screen';
  }
  focusWindow(){this.claimScreenId();super.focusWindow();}
  setState(state,label=state){
    this.state=state;this.status.textContent=label;this.root.dataset.state=state;
    // A resize scheduled while the guest was starting must not fire after it
    // failed or exited and overwrite the error/diagnostics surface.
    if(!['loading','running','waiting'].includes(state)){clearTimeout(this.resizeTimer);this.queuedGuestSize=null;}
    this.onStatus?.(state,label);
  }
  setError(text){
    this.error.textContent=text;
    this.onError?.(text);
  }
  close(){
    this.requestClose();
  }
  // Win32 close flow: send WM_CLOSE and let the guest's window procedure
  // destroy its window and post WM_QUIT. The worker stays alive until the
  // guest exits or the user forces Stop in the developer console. A second
  // click on the title-bar X disposes an already-stopped window.
  requestClose(){
    if(['loading','running','waiting'].includes(this.state)&&this.guestWindow){
      this.send(0x0010); // WM_CLOSE
    }else{
      this.dispose();
    }
  }
  onWindowMinimize(){
    if(this.guestWindow)this.send(0x0112,0xf020); // WM_SYSCOMMAND, SC_MINIMIZE
  }
  onWindowMaximize(maximized){
    if(this.guestWindow)this.send(0x0112,maximized?0xf030:0xf120); // SC_MAXIMIZE / SC_RESTORE
  }
  dispose(){
    this.stop();
    closeDialogsFor(this);
    this.removeUI();
    this.onClosed?.(this);
  }
  stop(){
    this.generation++;this.worker?.terminate();this.worker=null;this.guestWindow=0;
    this.pendingResize=null;this.queuedGuestSize=null;clearTimeout(this.resizeTimer);this.resizeDirty=false;
    closeDialogsFor(this);
    this.setState('stopped','Stopped');
  }
  queueGuestResize(width,height){
    const nextWidth=Math.max(1,Math.min(4096,Math.round(width))),nextHeight=Math.max(1,Math.min(4096,Math.round(height)));
    this.resizeDirty=true;this.desiredGuestSize=[nextWidth,nextHeight];
    if(this.state==='ready'||this.state==='stopped'||this.state==='exited'||this.state==='failed')return;
    this.queuedGuestSize=[nextWidth,nextHeight];
    clearTimeout(this.resizeTimer);this.resizeTimer=setTimeout(()=>this.flushGuestResize(),90);
  }
  flushGuestResize(){
    if(!this.queuedGuestSize)return;
    const [width,height]=this.queuedGuestSize;
    if(!this.send(RESIZE_GUEST_WINDOW,width,height))return;
    this.queuedGuestSize=null;this.pendingResize=[width,height];
    this.setError('');
  }
  resizeGuestToViewport(){
    if(this.screenWrap.clientWidth&&this.screenWrap.clientHeight)this.queueGuestResize(this.screenWrap.clientWidth,this.screenWrap.clientHeight);
  }
  send(message,wparam=0,lparam=0){
    if(!this.guestWindow)return false;
    if(message>=0x200&&message<=0x209)this.screen.dataset.lastPointer=`${message}:${this.guestWindow}:${lparam>>>0}`;
    try{enqueue(this.input,{window:this.guestWindow,message,wparam,lparam});return true;}catch(cause){this.setError(cause.message);this.stop();return false;}
  }
  rendererInfo(){
    return this.webglRenderer
      ? {backend:'WebGL',...this.webglRenderer.capabilities}
      : {backend:this.webglRequested?'WebGL unavailable':'CPU framebuffer'};
  }
  start(){
    this.stop();this.setError('');
    this.queuedGuestSize=this.desiredGuestSize?[...this.desiredGuestSize]:null;
    this.resizeDirty=Boolean(this.queuedGuestSize);
    this.setState('loading','Loading');
    if(this.webglRequested&&!this.webglRenderer){this.setError(this.webglInitError||'WebGL renderer is unavailable.');this.setState('failed','Failed');return;}
    this.pendingResize=null;clearTimeout(this.resizeTimer);
    if(!crossOriginIsolated||typeof SharedArrayBuffer==='undefined'){this.setError('Shared memory requires the local server with cross-origin isolation headers.');return;}
    this.input=new SharedArrayBuffer(BUFFER_BYTES);
    const gpuPacing=this.webglRequested?new SharedArrayBuffer(4):null;
    this.worker=new Worker(new URL('./worker.mjs',import.meta.url),{type:'module'});
    const current=this.generation;
    this.worker.onmessage=({data})=>{
      if(current!==this.generation)return;
      if(data.type==='diagnostic')this.publishDiagnostic(data.report);
      if(data.type==='dialog'){
        this.setState('waiting','Waiting for dialog');
        showGuestDialog(this,data,id=>{
          if(current!==this.generation)return;
          enqueue(this.input,{window:0,message:DIALOG_RESPONSE,wparam:id});
          this.setState('running','running');
        });
      }
      if(data.type==='gpu-frame'){
        try{
          this.webglRenderer.render(new Uint32Array(data.words));this.guestWindow=data.window;
          this.screen.dataset.webglFrames=String(Number(this.screen.dataset.webglFrames||0)+1);
          if(this.pendingResize&&this.pendingResize[0]===this.webglRenderer.canvas.width&&this.pendingResize[1]===this.webglRenderer.canvas.height){this.pendingResize=null;this.resizeDirty=false;}
          this.onSize?.(this.webglRenderer.canvas.width,this.webglRenderer.canvas.height);
          this.flushGuestResize();
          requestAnimationFrame(()=>{if(current===this.generation&&gpuPacing){Atomics.store(new Int32Array(gpuPacing),0,data.sequence+1);Atomics.notify(new Int32Array(gpuPacing),0);}});
        }catch(cause){
          this.setError(cause.stack??cause.message);this.setState('failed','Failed');
          this.generation++;this.worker?.terminate();this.worker=null;
        }
      }
      if(data.type==='frame'&&!this.webglRequested){
        this.guestWindow=data.window;this.screen.width=data.width;this.screen.height=data.height;
        this.screen.dataset.guestWindow=String(data.window);
        this.screen.getContext('2d').putImageData(new ImageData(data.pixels,data.width,data.height),0,0);
        if(this.pendingResize&&this.pendingResize[0]===data.width&&this.pendingResize[1]===data.height){this.pendingResize=null;this.resizeDirty=false;}
        this.onSize?.(data.width,data.height);
        this.flushGuestResize();
      }
      if(data.type==='status'){
        if(data.state==='exited'){this.guestWindow=0;this.setState('exited',`Exited (${data.code})`);}
        else this.setState(data.state,data.state);
      }
      if(data.type==='error'){
        this.setError(this.debugReport?.reason??data.message.split('\n')[0]);this.setState('failed','Failed');
        this.publishDiagnostic({...this.debugReport,kind:'failure',reason:this.debugReport?.reason??data.message.split('\n')[0],runtimeError:data.message,javascriptStack:data.stack});
      }
    };
    this.worker.onerror=event=>{
      this.setError(event.message);this.setState('failed','Failed');
      this.publishDiagnostic({...this.debugReport,kind:'failure',reason:event.message,workerError:{file:event.filename,line:event.lineno,column:event.colno}});
    };
    const hostProfile=this.webglRequested?'release':'debug';
    this.worker.postMessage({manifestURL:this.manifestURLValue,hostURL:new URL(`/target/wasm32-unknown-unknown/${hostProfile}/wlam_browser_host.wasm`,location.href).href,input:this.input,webgl:this.webglRequested,gpuPacing});
  }
  publishDiagnostic(report){
    this.debugReport={...report,runner:{url:location.href,userAgent:navigator.userAgent,crossOriginIsolated,manifest:this.manifestURLValue,renderer:this.rendererInfo()}};
    this.onDebug?.(this.debugReport);
  }
  bindGuestInput(){
    this.screen.onpointerdown=event=>{
      this.screen.focus();this.screen.setPointerCapture(event.pointerId);
      const rect=this.screen.getBoundingClientRect();
      const x=Math.floor((event.clientX-rect.left)*this.screen.width/rect.width),y=Math.floor((event.clientY-rect.top)*this.screen.height/rect.height);
      this.send(0x201,1,(x&65535)|((y&65535)<<16));
    };
    this.screen.onpointerup=event=>{
      const rect=this.screen.getBoundingClientRect();
      const x=Math.floor((event.clientX-rect.left)*this.screen.width/rect.width),y=Math.floor((event.clientY-rect.top)*this.screen.height/rect.height);
      this.send(0x202,0,(x&65535)|((y&65535)<<16));
      if(this.screen.hasPointerCapture(event.pointerId))this.screen.releasePointerCapture(event.pointerId);
    };
    this.screen.onpointercancel=()=>this.send(31);
    this.screen.onkeydown=event=>{const key=keycode(event);if(key!==null){event.preventDefault();this.send(0x100,key);}};
    this.screen.onkeyup=event=>{const key=keycode(event);if(key!==null){event.preventDefault();this.send(0x101,key);}};
  }
}

// Development-only surface for arbitrary manifests, forced stops, manual guest
// sizes and diagnostics. It never contains guest pixels; guests stay bare.
class DevConsole extends DesktopWindow{
  constructor(){
    const root=document.querySelector('#app-window');
    super({
      root,title:'Developer Console',icon:'🛠️',
      taskButton:document.querySelector('#task-app'),
      restoreItem:document.querySelector('#menu-runner'),
      restartItem:document.querySelector('#menu-restart'),
    });
    const role=name=>root.querySelector(`[data-role="${name}"]`);
    this.manifestInput=role('manifest');
    this.startButton=role('start');this.stopButton=role('stop');this.closeButton=role('close-guest');
    this.resizeButton=role('resize-guest');this.resizeWidth=role('guest-width');this.resizeHeight=role('guest-height');
    this.status=role('status');this.statusDot=role('status-dot');this.error=role('error');
    this.debugPanel=role('debugger');this.downloadDebug=role('download-debug');
    this.debugSummary=role('debug-summary');this.debugReportElement=role('debug-report');
    this.guest=null;this.debugReport=null;this.open=false;this.resizeDirty=false;
    this.manifestInput.value=urlManifest??DEFAULT_MANIFEST;
    new MutationObserver(()=>this.statusDot.classList.toggle('live',this.status.textContent==='running')).observe(this.status,{childList:true,characterData:true,subtree:true});
    this.restart=()=>this.start();
    this.bindConsole();
  }
  openConsole(){
    this.root.hidden=false;this.taskButton.hidden=false;this.open=true;
    const guest=windows
      .filter(window=>window instanceof GuestWindow&&window.root.isConnected)
      .sort((a,b)=>(Number(b.root.style.zIndex)||0)-(Number(a.root.style.zIndex)||0))[0];
    if(guest){
      const manifestURL=new URL(guest.manifestURLValue);
      this.manifestInput.value=manifestURL.origin===location.origin?manifestURL.pathname:manifestURL.href;
      this.attach(guest);
      this.setStatus(guest.state,guest.status.textContent);
    }
    this.reveal();
  }
  close(){this.markWindowState('minimized');}
  rendererFor(){return urlRenderer==='webgl'?'webgl':null;}
  setStatus(state,label=state){
    this.status.textContent=label;
    const live=state==='running'||state==='waiting';
    this.stopButton.disabled=!live;
    this.closeButton.disabled=!live;
    this.resizeButton.disabled=!live;
  }
  attach(guest){
    this.guest=guest;
    guest.onStatus=(state,label)=>this.setStatus(state,label);
    guest.onDebug=report=>this.updateDebug(report);
    guest.onError=text=>{this.error.textContent=text;};
    guest.onSize=(width,height)=>{
      if(this.resizeDirty)return;
      this.resizeWidth.value=width;this.resizeHeight.value=height;
    };
    guest.onClosed=()=>{if(this.guest===guest)this.guest=null;};
    this.resizeWidth.value=guest.screen.width;this.resizeHeight.value=guest.screen.height;
  }
  start(){
    const manifest=this.manifestInput.value.trim()||DEFAULT_MANIFEST;
    const manifestURL=new URL(manifest,location.href).href;
    this.error.textContent='';
    this.resizeDirty=false;
    // Reuse a running window for the same manifest, whether it was opened
    // from a desktop shortcut or from here, so a bundle never starts twice.
    let guest=windows.find(window=>window instanceof GuestWindow&&window.manifestURLValue===manifestURL);
    if(!guest){
      guest=new GuestWindow({title:titleFromManifest(manifest),icon:'🖥️',manifest,renderer:this.rendererFor()});
    }
    this.attach(guest);
    guest.reveal();guest.resizeGuestToViewport();guest.start();
  }
  stop(){this.guest?.stop();}
  applyGuestSize(){
    const width=Number(this.resizeWidth.value),height=Number(this.resizeHeight.value);
    if(!Number.isInteger(width)||!Number.isInteger(height)||width<1||height<1||width>4096||height>4096){
      this.error.textContent='Guest dimensions must be between 1 and 4096 pixels.';return;
    }
    if(!this.guest)return;
    this.error.textContent='';this.resizeDirty=true;
    this.guest.desiredGuestSize=[width,height];this.guest.queuedGuestSize=null;
    this.guest.send(RESIZE_GUEST_WINDOW,width,height);
    this.guest.pendingResize=[width,height];this.guest.resizeDirty=true;
  }
  updateDebug(report){
    this.debugReport=report;
    this.downloadDebug.disabled=false;
    const api=report.failedApi;
    this.debugSummary.textContent=[
      report.kind==='failure'?(report.reason??'Guest execution failed'):`Last snapshot: ${report.kind}`,
      api?`Failed API: ${api.library}!${api.name}(${api.args.map(hex).join(', ')})${api.argumentCountKnown===false?' [raw stack words; argument count unknown]':''}`:'',
      api?`Guest return address: ${hex(api.returnAddress)}`:'',
      report.runner.renderer.backend==='WebGL'?`WebGL depth: ${report.runner.renderer.depthBits} bits; subpixel precision: ${report.runner.renderer.subpixelBits} bits.`:'CPU framebuffer renderer.',
      report.guest?Object.entries(report.guest.registers).map(([name,value])=>`${name}=${hex(value)}`).join(' '):'',
      `${report.recentApis?.length??0} recent API calls captured.`,
    ].filter(Boolean).join('\n');
    this.debugReportElement.textContent=formatDebugReport(report);
    if(report.kind==='failure')this.debugPanel.open=true;
  }
  bindConsole(){
    this.startButton.onclick=()=>this.start();
    this.stopButton.onclick=()=>this.stop();
    this.closeButton.onclick=()=>this.guest?.send(0x10);
    this.resizeButton.onclick=()=>this.applyGuestSize();
    this.resizeWidth.oninput=this.resizeHeight.oninput=()=>{this.resizeDirty=true;};
    this.downloadDebug.onclick=()=>{
      if(!this.debugReport)return;
      const url=URL.createObjectURL(new Blob([formatDebugReport(this.debugReport)],{type:'application/json'}));
      const link=document.createElement('a');link.href=url;link.download=`wlam-debug-${new Date().toISOString().replace(/[:.]/g,'-')}.json`;
      document.body.append(link);link.click();link.remove();setTimeout(()=>URL.revokeObjectURL(url),1000);
    };
  }
}

const RECOMPILE_STEPS=['source','analysis','build','report'];

// Drag-and-drop recompilation. The wizard talks to the local dev server, which
// analyzes and recompiles with native wlam subprocesses; the page never sees or
// executes the uploaded PE itself.
class RecompileWizard{
  constructor(){
    this.dialog=document.querySelector('#recompile-dialog');
    this.stepItems=[...document.querySelectorAll('#recompile-steps [data-step]')];
    this.panels=[...this.dialog.querySelectorAll('[data-panel]')];
    this.fileSummary=document.querySelector('#recompile-file');
    this.exeSelect=document.querySelector('#recompile-exe');
    this.assets=document.querySelector('#recompile-assets');
    this.dlls=document.querySelector('#recompile-dlls');
    this.dllRow=document.querySelector('#recompile-dll-row');
    this.dllList=document.querySelector('#recompile-dll-list');
    this.dllHint=document.querySelector('#recompile-dll-hint');
    this.sourceError=document.querySelector('#recompile-source-error');
    this.installerPanel=document.querySelector('#recompile-installer');
    this.analysisPanel=document.querySelector('#recompile-analysis');
    this.progress=document.querySelector('#recompile-progress');
    this.log=document.querySelector('#recompile-log');
    this.buildError=document.querySelector('#recompile-build-error');
    this.reportPanel=document.querySelector('#recompile-report');
    this.status=document.querySelector('#recompile-status');
    this.closeButton=document.querySelector('#recompile-close');
    this.backButton=document.querySelector('#recompile-back');
    this.nextButton=document.querySelector('#recompile-next');
    this.job=null;this.step='source';this.busy=false;this.buildFailed=false;this.timer=0;this.pollMode='';
    this.bind();
  }
  bind(){
    this.closeButton.onclick=()=>this.close();
    this.backButton.onclick=()=>this.back();
    this.nextButton.onclick=()=>this.next();
    this.exeSelect.onchange=()=>{this.status.textContent='';this.sourceError.textContent='';};
    this.assets.onchange=()=>{this.status.textContent='';this.sourceError.textContent='';};
    this.dlls.onchange=()=>{this.status.textContent='';this.sourceError.textContent='';};
    this.dialog.oncancel=event=>{event.preventDefault();this.close();};
  }
  setStep(step){
    this.step=step;
    for(const panel of this.panels)panel.hidden=panel.dataset.panel!==step;
    const order=RECOMPILE_STEPS.indexOf(step);
    for(const item of this.stepItems){
      const index=RECOMPILE_STEPS.indexOf(item.dataset.step);
      item.classList.toggle('active',index===order);
      item.classList.toggle('done',index<order);
    }
    this.renderActions();
  }
  renderActions(){
    const step=this.step,busy=this.busy,blockers=this.job?.analysis?.blocking?.length??0;
    this.closeButton.disabled=busy;
    this.backButton.hidden=step==='source'||step==='build';
    this.backButton.textContent=step==='report'?'Close':'Back';
    this.backButton.disabled=busy;
    this.nextButton.hidden=false;
    if(step==='source'){
      this.nextButton.textContent='Inspect compatibility';
      this.nextButton.disabled=busy||!this.job||!(this.job.executables??[]).length;
    }else if(step==='analysis'){
      this.nextButton.textContent='Recompile';
      this.nextButton.disabled=busy||blockers>0||!this.job?.analysis;
    }else if(step==='build'){
      this.nextButton.textContent=this.buildFailed?'Back to compatibility':busy?'Recompiling…':'Continue';
      this.nextButton.disabled=busy;
    }else{
      this.nextButton.textContent='Open on desktop';
      this.nextButton.disabled=busy||!this.job?.bundle;
    }
  }
  back(){
    if(this.busy)return;
    if(this.step==='report'){this.close();return;}
    if(this.step==='build'&&this.buildFailed){this.buildFailed=false;this.setStep('analysis');return;}
    if(this.step==='analysis'){this.setStep('source');this.status.textContent='';}
  }
  next(){
    if(this.busy)return;
    if(this.step==='source'){this.analyze();return;}
    if(this.step==='analysis'){this.build();return;}
    if(this.step==='build'&&this.buildFailed){this.buildFailed=false;this.setStep('analysis');return;}
    if(this.step==='report'&&this.job?.bundle){
      const bundle=this.job.bundle;
      this.close();
      openBundle(bundle);
    }
  }
  async open(file,mode='auto'){
    if(!file)return;
    if(!recompileAvailable){toast(recompileUnavailable(),10000);return;}
    if(file.size>recompileConfig.maxUpload){toast(`Upload is larger than the ${formatBytes(recompileConfig.maxUpload)} upload limit.`);return;}
    if(this.busy){toast('Wait for the current recompile to finish or fail.');return;}
    if(this.dialog.open)this.close();
    this.job=null;this.busy=true;this.buildFailed=false;
    this.sourceError.textContent='';this.analysisPanel.replaceChildren();
    this.installerPanel.hidden=true;this.installerPanel.replaceChildren();
    this.progress.replaceChildren();this.log.textContent='';this.log.hidden=true;
    this.buildError.textContent='';this.reportPanel.replaceChildren();
    this.exeSelect.replaceChildren();
    this.fileSummary.textContent=`Importing ${file.name}…`;
    this.status.textContent='Reading archive';
    this.setStep('source');
    if(!this.dialog.open)this.dialog.showModal();
    try{
      const job=await apiRequest(RECOMPILE_API,{
        method:'POST',
        headers:{'Content-Type':'application/octet-stream','X-Filename':encodeURIComponent(file.name),'X-Mode':mode},
        body:file,
      });
      this.job=job;
      this.fileSummary.textContent=`${job.archive} · ${job.archiveEntries} entries · ${formatBytes(job.archiveBytes)} compressed · ${formatBytes(job.expandedBytes)} expanded`;
      this.showInstaller(job);
      this.exeSelect.replaceChildren(...job.executables.map(name=>{
        const option=uiNode('option',null,name);
        option.value=name;
        return option;
      }));
      this.exeSelect.value=job.selected;
      this.assets.checked=Boolean(job.includeAssets);
      const dlls=job.dlls??[];
      this.dllRow.hidden=!dlls.length;
      this.dllHint.hidden=!dlls.length;
      this.dlls.checked=job.includeDlls!==false;
      this.dllList.textContent=dlls.length?`(${dlls.map(name=>name.split('/').pop()).join(', ')})`:'';
      this.dllRow.title=dlls.join('\n');
      this.status.textContent=`${job.executables.length} executable${job.executables.length===1?'':'s'} found`;
    }catch(error){
      this.sourceError.textContent=error.message;
      this.status.textContent='Import failed';
    }finally{
      this.busy=false;
      this.renderActions();
    }
  }
  close(){
    if(this.busy){toast('Wait for the current recompile to finish or fail.');return;}
    this.stopPolling();
    if(this.dialog.open)this.dialog.close();
    const job=this.job;
    this.job=null;this.busy=false;this.buildFailed=false;
    if(job&&['review','analyzed','ready','failed'].includes(job.state)){
      fetch(`${RECOMPILE_API}/${job.id}`,{method:'DELETE'}).catch(()=>{});
    }
  }
  stopPolling(){clearInterval(this.timer);this.timer=0;}
  showAnalysisError(message){
    this.analysisPanel.replaceChildren(uiNode('h3',null,'Compatibility'),uiNode('div','wizard-error',message));
  }
  async analyze(){
    if(!this.job||this.busy)return;
    this.busy=true;this.buildFailed=false;this.sourceError.textContent='';
    this.analysisPanel.replaceChildren(uiNode('p','wizard-hint','Reading the PE import table…'));
    this.status.textContent='Analyzing image…';
    this.setStep('analysis');
    try{
      const job=await apiRequest(`${RECOMPILE_API}/${this.job.id}/analyze`,{
        method:'POST',
        headers:{'Content-Type':'application/json'},
        body:JSON.stringify({selected:this.exeSelect.value,includeAssets:this.assets.checked,includeDlls:this.dlls.checked}),
      });
      this.job=job;
      this.startPolling('analysis');
    }catch(error){
      this.busy=false;
      if(this.job)this.job.analysis=null;
      this.showAnalysisError(error.message);
      this.status.textContent='Analysis not started';
      this.renderActions();
    }
  }
  async build(){
    if(!this.job||this.busy)return;
    this.busy=true;this.buildFailed=false;this.buildError.textContent='';
    this.status.textContent='Starting recompiler…';
    this.renderBuild(this.job);
    this.setStep('build');
    try{
      const job=await apiRequest(`${RECOMPILE_API}/${this.job.id}/build`,{
        method:'POST',headers:{'Content-Type':'application/json'},body:'{}',
      });
      this.job=job;
      this.startPolling('build');
    }catch(error){
      this.busy=false;this.buildFailed=true;
      this.buildError.textContent=error.message;
      this.status.textContent='Recompile not started';
      this.renderActions();
    }
  }
  startPolling(mode){
    this.pollMode=mode;
    this.stopPolling();
    const tick=async()=>{
      if(!this.job)return;
      try{
        const job=await apiRequest(`${RECOMPILE_API}/${this.job.id}`);
        this.job=job;
        if(mode==='analysis'){
          this.status.textContent=job.state==='analyzing'?'Analyzing image…':'Checking analysis';
          if(job.state==='analyzed'){
            this.stopPolling();this.busy=false;
            this.renderAnalysis();
            this.setStep('analysis');
            this.status.textContent='Analysis complete';
            return;
          }
        }else{
          this.renderBuild(job);
          this.status.textContent=job.state==='compiling'?'Recompiling…':'Packaging bundle';
          if(job.state==='ready'){
            this.stopPolling();this.busy=false;
            this.finish(job);
            return;
          }
        }
        if(job.state==='failed'){
          this.stopPolling();this.busy=false;
          if(mode==='analysis'){
            this.showAnalysisError(job.error??'Image analysis failed');
            this.status.textContent='Analysis failed';
            this.setStep('analysis');
          }else{
            this.buildFailed=true;
            this.buildError.textContent=job.error??'Recompile failed';
            this.status.textContent='Recompile failed';
            this.renderActions();
          }
        }
      }catch(error){
        this.stopPolling();this.busy=false;this.buildFailed=mode==='build';
        if(mode==='analysis'){
          if(this.job)this.job.analysis=null;
          this.showAnalysisError(error.message);
        }else this.buildError.textContent=error.message;
        this.status.textContent='Job failed';
        this.renderActions();
      }
    };
    this.timer=setInterval(tick,500);
    tick();
  }
  showInstaller(job){
    const installer=job?.installer;
    if(!installer)return;
    const fragment=document.createDocumentFragment();
    const label=installer.format
      ? `Installer detected: ${installer.format}${installer.tool?` · ${installer.tool}`:''}`
      : 'Installer detected';
    fragment.append(uiNode('p','wizard-hint',label));
    if(installer.source)fragment.append(uiNode('p','wizard-hint',`Stub: ${installer.source}`));
    for(const warning of installer.warnings??[])fragment.append(uiNode('p','wizard-hint',warning));
    if(installer.needsRun){
      fragment.append(uiNode('p','wizard-error',
        'No static payload was found. This installer only unpacks when executed, so upload an already-extracted folder as a ZIP instead.'));
    }
    this.installerPanel.replaceChildren(fragment);
    this.installerPanel.hidden=false;
  }
  renderAnalysis(){
    const analysis=this.job?.analysis??{};
    const blockers=Array.isArray(analysis.blocking)?analysis.blocking:[];
    const fragment=document.createDocumentFragment();
    fragment.append(uiNode('h3',null,'Compatibility'));
    const stats=uiNode('div','stat-grid');
    for(const [label,value] of [['Imports',analysis.imports??0],['Libraries',(analysis.libraries??[]).length],['Blockers',blockers.length],['Format',String(analysis.kind??'?').toUpperCase()]]){
      const cell=uiNode('div');
      cell.append(uiNode('strong',null,String(value)),uiNode('span',null,label));
      stats.append(cell);
    }
    fragment.append(stats);
    if(analysis.machine)fragment.append(uiNode('p','wizard-hint',`Machine ${analysis.machine} · image base ${analysis.image_base??'unknown'} · entry RVA ${analysis.entry_point??'unknown'}`));
    const compatibility=Object.entries(analysis.compatibility??{});
    if(compatibility.length)fragment.append(uiNode('p',null,`Compatibility: ${compatibility.map(([state,count])=>`${state} ${count}`).join(' · ')}`));
    if(blockers.length){
      fragment.append(uiNode('p',null,`${blockers.length} blocking import${blockers.length===1?'':'s'} must be resolved before recompiling.`));
      const table=uiNode('table','wizard-table');
      const head=uiNode('tr');
      for(const label of ['Library','Symbol','State','Reason'])head.append(uiNode('th',null,label));
      table.append(head);
      for(const blocker of blockers){
        const row=uiNode('tr');
        row.append(uiNode('td',null,blocker.library??''),uiNode('td',null,blocker.symbol??''),uiNode('td',null,blocker.state??''),uiNode('td',null,blocker.reason??''));
        table.append(row);
      }
      fragment.append(table);
    }else{
      fragment.append(uiNode('p',null,'No blocking imports reported. A clean import report does not prove instruction, graphics, audio or filesystem compatibility.'));
    }
    if((analysis.libraries??[]).length){
      const table=uiNode('table','wizard-table');
      const head=uiNode('tr');head.append(uiNode('th',null,'Imported library'),uiNode('th',null,'Symbols'));table.append(head);
      for(const library of analysis.libraries){
        const row=uiNode('tr');row.append(uiNode('td',null,library.name??''),uiNode('td',null,String(library.symbols??'')));table.append(row);
      }
      fragment.append(table);
    }
    this.analysisPanel.replaceChildren(fragment);
  }
  renderBuild(job){
    this.progress.replaceChildren(...(job.steps??[]).map(step=>{
      const item=uiNode('li',step.state??'pending');
      const mark=step.state==='done'?'✓':step.state==='failed'?'!':step.state==='running'?'◌':'○';
      item.append(uiNode('span','mark',mark),uiNode('span',null,step.name??''));
      if(step.state==='running')item.append(uiNode('span','detail','working…'));
      return item;
    }));
    this.log.textContent=job.log??'';
    this.log.hidden=!job.log;
  }
  finish(job){
    addBundleShortcut(job.bundle);
    this.renderReport(job);
    this.setStep('report');
    this.status.textContent=`${job.name} added to the desktop`;
    loadBundleCatalog();
  }
  renderReport(job){
    const report=job.report??{};
    const bundle=job.bundle??{};
    const analysis=report.analysis??job.analysis??{};
    const fragment=document.createDocumentFragment();
    fragment.append(uiNode('h3',null,'Recompile complete'));
    fragment.append(uiNode('p','wizard-hint',`Bundle and report written to target/${bundle.id}/bundle. A shortcut is already on the desktop.`));
    const stats=uiNode('div','stat-grid');
    for(const [label,value] of [
      ['Imports',analysis.imports??0],
      ['Blockers',(analysis.blocking??[]).length],
      ['Build time',`${report.build?.seconds??'—'} s`],
      ['Assets',(report.assets??[]).length],
      ['DLLs',(report.dlls??[]).length],
    ]){
      const cell=uiNode('div');
      cell.append(uiNode('strong',null,String(value)),uiNode('span',null,label));
      stats.append(cell);
    }
    fragment.append(stats);
    if(report.installer){
      const installer=report.installer;
      const line=[installer.format,installer.tool].filter(Boolean).join(' · ');
      if(line)fragment.append(uiNode('p','wizard-hint',`Installer: ${line}`));
      for(const warning of installer.warnings??[])fragment.append(uiNode('p','wizard-hint',warning));
    }
    if((report.dlls??[]).length){
      fragment.append(uiNode('p','wizard-hint',`Linked DLLs: ${report.dlls.map(dll=>`${dll.name} @ 0x${(dll.base>>>0).toString(16)}`).join(', ')}`));
    }
    const artifacts=report.bundle?.artifacts??[];
    if(artifacts.length){
      const table=uiNode('table','wizard-table');
      const head=uiNode('tr');
      for(const label of ['Artifact','Bytes','SHA-256'])head.append(uiNode('th',null,label));
      table.append(head);
      for(const artifact of artifacts){
        const row=uiNode('tr');
        row.append(uiNode('td',null,artifact.path??''),uiNode('td',null,String(artifact.bytes??'')),uiNode('td','hash',artifact.sha256??''));
        table.append(row);
      }
      fragment.append(table);
    }
    const links=uiNode('div','report-links');
    if(bundle.report){
      const json=uiNode('a',null,'Download JSON report');
      json.href=new URL(bundle.report,location.href).href;json.download='report.json';
      links.append(json);
    }
    if(bundle.reportMarkdown){
      const markdown=uiNode('a',null,'Download Markdown report');
      markdown.href=new URL(bundle.reportMarkdown,location.href).href;markdown.download='report.md';
      links.append(markdown);
    }
    if(bundle.matrix){
      const matrix=uiNode('a',null,'Download compatibility matrix');
      matrix.href=new URL(bundle.matrix,location.href).href;matrix.download='compatibility.md';
      links.append(matrix);
    }
    if(bundle.manifest){
      const manifest=uiNode('a',null,'Open manifest');
      manifest.href=new URL(bundle.manifest,location.href).href;manifest.target='_blank';manifest.rel='noopener';
      links.append(manifest);
    }
    fragment.append(links);
    fragment.append(uiNode('p','wizard-hint','This report records static analysis and build output. It is not a verified runtime run.'));
    const details=uiNode('details');
    details.append(uiNode('summary',null,'Full report (JSON)'),uiNode('pre','report-json',JSON.stringify(report,null,2)));
    fragment.append(details);
    this.reportPanel.replaceChildren(fragment);
  }
}

const consoleApp=new DevConsole();
consoleApp.root.hidden=true;
consoleApp.taskButton.hidden=true;
const recompileWizard=new RecompileWizard();

const BUNDLE_GLYPHS={interactive:'🖥️',tolympic:'🏅',hover:'🚁',gdi:'🎨','opengl-fixture':'✨'};
const glyphFor=bundle=>bundle.icon??BUNDLE_GLYPHS[bundle.id]??'🧩';
const restartable=guest=>!['loading','running','waiting'].includes(guest.state);
function openBundle(bundle){
  const manifestURL=new URL(bundle.manifest,location.href).href;
  const existing=windows.find(window=>window instanceof GuestWindow&&window.manifestURLValue===manifestURL);
  if(existing){
    existing.reveal();
    if(restartable(existing))existing.start();
    return;
  }
  const title=bundle.name??bundle.id??titleFromManifest(bundle.manifest);
  const guest=new GuestWindow({title,icon:glyphFor(bundle),manifest:bundle.manifest,renderer:bundle.renderer??null});
  guest.positionCascade();
  guest.reveal();
  guest.resizeGuestToViewport();
  guest.start();
}
function addBundleShortcut(bundle){
  if(!bundle?.manifest)return;
  const id=bundle.id??bundle.manifest;
  if([...desktopIcons.querySelectorAll('[data-bundle]')].some(icon=>icon.dataset.bundle===id))return;
  const button=document.createElement('button');
  button.className='desktop-icon';
  button.dataset.bundle=id;
  if(bundle.report){
    button.dataset.report=bundle.report;
    button.title=`Recompile report: ${bundle.report}`;
  }
  const glyph=document.createElement('span');glyph.className='glyph';glyph.setAttribute('aria-hidden','true');glyph.textContent=glyphFor(bundle);
  const label=document.createElement('span');label.textContent=bundle.name??bundle.id??'Bundle';
  button.append(glyph,label);
  button.onclick=()=>openBundle(bundle);
  desktopIcons.append(button);
}
async function loadBundleCatalog(){
  try{
    const response=await fetch(new URL('./bundles.json',import.meta.url),{cache:'no-store'});
    if(!response.ok)return;
    const catalog=await response.json();
    const bundles=Array.isArray(catalog)?catalog:catalog.bundles??[];
    desktopIcons.querySelectorAll('[data-bundle]').forEach(icon=>icon.remove());
    for(const bundle of bundles)addBundleShortcut(bundle);
  }catch{
    // Static hosting without the dev-server scan endpoint simply keeps the
    // developer console shortcut; manifest paths can still be typed manually.
  }
}

let recompileConfig=null,recompileAvailable=false;
function recompileUnavailable(){
  if(recompileConfig&&!recompileConfig.available)return `Recompilation is unavailable: wlam binary not found at ${recompileConfig.binary}. Build wlam-cli or set WLAM_BIN, then restart the dev server.`;
  return 'Recompilation is unavailable. Start python3 scripts/serve-web.py instead of a static file server.';
}
function showRecompileControls(visible){
  document.querySelector('#desktop-recompile').hidden=!visible;
  document.querySelector('#menu-recompile').hidden=!visible;
}
async function loadRecompileConfig(){
  try{
    recompileConfig=await apiRequest(`${RECOMPILE_API}/config`);
    recompileAvailable=Boolean(recompileConfig.available);
    showRecompileControls(true);
  }catch{
    recompileConfig=null;recompileAvailable=false;
    showRecompileControls(false);
  }
}
const recompileFileInput=document.querySelector('#recompile-file-input');
function chooseArchive(){
  if(!recompileConfig||!recompileAvailable){toast(recompileUnavailable(),10000);return;}
  recompileFileInput.value='';
  recompileFileInput.click();
}
document.querySelector('#desktop-recompile').onclick=chooseArchive;
document.querySelector('#menu-recompile').onclick=()=>{toggleMenu(false);chooseArchive();};
recompileFileInput.onchange=()=>{
  const file=recompileFileInput.files?.[0];
  if(file)recompileWizard.open(file);
};
let dragDepth=0;
const hasFiles=event=>[...(event.dataTransfer?.types??[])].includes('Files');
document.addEventListener('dragenter',event=>{
  if(!hasFiles(event))return;
  event.preventDefault();
  if(!recompileAvailable)return;
  dragDepth++;
  dropOverlay.hidden=false;
});
document.addEventListener('dragover',event=>{
  if(!hasFiles(event))return;
  event.preventDefault();
  if(event.dataTransfer)event.dataTransfer.dropEffect=recompileAvailable?'copy':'none';
});
document.addEventListener('dragleave',event=>{
  if(dropOverlay.hidden)return;
  dragDepth=Math.max(0,dragDepth-1);
  if(!dragDepth)dropOverlay.hidden=true;
});
document.addEventListener('drop',event=>{
  if(!hasFiles(event))return;
  event.preventDefault();
  dragDepth=0;dropOverlay.hidden=true;
  if(!recompileAvailable){toast(recompileUnavailable(),10000);return;}
  const file=event.dataTransfer?.files?.[0];
  if(!file)return;
  recompileWizard.open(file);
});

function toggleMenu(open=!startMenu.classList.contains('open')){startMenu.classList.toggle('open',open);startMenuButton.setAttribute('aria-expanded',String(open));}
startMenuButton.onclick=()=>toggleMenu();
document.querySelector('#menu-refresh-bundles').onclick=()=>{toggleMenu(false);loadBundleCatalog();};
document.querySelector('#desktop-launch').onclick=()=>consoleApp.openConsole();
document.querySelector('#menu-runner').onclick=()=>consoleApp.openConsole();
document.querySelector('#menu-restart').onclick=()=>{consoleApp.openConsole();consoleApp.start();};
document.addEventListener('pointerdown',event=>{if(!startMenu.contains(event.target)&&event.target!==startMenuButton)toggleMenu(false);});
const clock=document.querySelector('#task-clock');
const updateClock=()=>clock.textContent=new Intl.DateTimeFormat(undefined,{hour:'numeric',minute:'2-digit'}).format(new Date());
updateClock();setInterval(updateClock,15000);
globalThis.addEventListener('resize',()=>requestAnimationFrame(()=>{
  for(const window of windows){
    if(window.root.hidden||window.root.classList.contains('is-minimized'))continue;
    window.fitToDesktop();window.resizeGuestToViewport?.();
  }
}));
loadBundleCatalog();
loadRecompileConfig();
if(urlManifest||urlParameters.has('renderer')||urlParameters.has('dev'))consoleApp.openConsole();
