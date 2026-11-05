import {BUFFER_BYTES,enqueue,RESIZE_GUEST_WINDOW} from './queue.mjs';
import {dialogButtons,DIALOG_RESPONSE} from './dialog.mjs';
import {WebGlRenderer} from './webgl-renderer.mjs';
const dialog=document.querySelector('#guest-dialog');
const screen=document.querySelector('#screen'),status=document.querySelector('#status'),error=document.querySelector('#error');
const stopButton=document.querySelector('#stop'),closeButton=document.querySelector('#close');
const resizeButton=document.querySelector('#resize-guest');
const resizeWidth=document.querySelector('#guest-width'),resizeHeight=document.querySelector('#guest-height');
const manifest=new URLSearchParams(location.search).get('manifest');
if(manifest)document.querySelector('#manifest').value=manifest;
const webglRequested=new URLSearchParams(location.search).get('renderer')==='webgl';
let webglRenderer=null,webglInitError='';
if(webglRequested){try{webglRenderer=new WebGlRenderer(screen);}catch(cause){webglInitError=cause.message;}}
let worker,input,window=0,generation=0,resizeDirty=false,pendingResize=null;
function stop(){generation++;worker?.terminate();worker=null;window=0;pendingResize=null;resizeDirty=false;dialog.close();stopButton.disabled=true;closeButton.disabled=true;resizeButton.disabled=true;status.textContent='Stopped';}
function send(message,wparam=0,lparam=0){if(!window)return;try{enqueue(input,{window,message,wparam,lparam});}catch(cause){error.textContent=cause.message;stop();}}
document.querySelector('#start').onclick=()=>{
  stop();error.textContent='';
  if(webglRequested&&!webglRenderer){error.textContent=webglInitError||'WebGL renderer is unavailable.';status.textContent='Failed';return;}
  resizeDirty=false;pendingResize=null;
  if(!crossOriginIsolated || typeof SharedArrayBuffer==='undefined'){error.textContent='Shared memory requires the local server with cross-origin isolation headers.';return;}
  input=new SharedArrayBuffer(BUFFER_BYTES);const gpuPacing=webglRequested?new SharedArrayBuffer(4):null;worker=new Worker(new URL('./worker.mjs',import.meta.url),{type:'module'});
  const current=generation;stopButton.disabled=false;status.textContent='Loading';
  worker.onmessage=({data})=>{
    if(current!==generation)return;
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
        requestAnimationFrame(()=>{if(current===generation&&gpuPacing){Atomics.store(new Int32Array(gpuPacing),0,data.sequence+1);Atomics.notify(new Int32Array(gpuPacing),0);}});
      }catch(cause){error.textContent=cause.stack??cause.message;status.textContent='Failed';generation++;worker?.terminate();worker=null;stopButton.disabled=true;closeButton.disabled=true;resizeButton.disabled=true;}
    }
    if(data.type==='frame'&&!webglRequested){window=data.window;screen.width=data.width;screen.height=data.height;screen.getContext('2d').putImageData(new ImageData(data.pixels,data.width,data.height),0,0);closeButton.disabled=false;resizeButton.disabled=false;if(pendingResize&&pendingResize[0]===data.width&&pendingResize[1]===data.height){pendingResize=null;resizeDirty=false;}if(!resizeDirty){resizeWidth.value=data.width;resizeHeight.value=data.height;}}
    if(data.type==='status'){status.textContent=data.state==='exited'?`Exited (${data.code})`:data.state;if(data.state==='exited'){stopButton.disabled=true;closeButton.disabled=true;window=0;}}
    if(data.type==='error'){error.textContent=data.message;status.textContent='Failed';closeButton.disabled=true;}
  };
  worker.onerror=event=>{error.textContent=event.message;status.textContent='Failed';};
  const hostProfile=webglRequested?'release':'debug';
  worker.postMessage({manifestURL:new URL(document.querySelector('#manifest').value,location.href).href,hostURL:new URL(`/target/wasm32-unknown-unknown/${hostProfile}/wlam_browser_host.wasm`,location.href).href,input,webgl:webglRequested,gpuPacing});
};
stopButton.onclick=stop;closeButton.onclick=()=>send(0x10);
resizeButton.onclick=()=>{
  const width=Number(resizeWidth.value),height=Number(resizeHeight.value);
  if(!Number.isInteger(width)||!Number.isInteger(height)||width<1||height<1||width>4096||height>4096){error.textContent='Guest dimensions must be between 1 and 4096 pixels.';return;}
  error.textContent='';send(RESIZE_GUEST_WINDOW,width,height);
  pendingResize=[width,height];resizeDirty=true;
};
resizeWidth.oninput=resizeHeight.oninput=()=>{resizeDirty=true;};
screen.onpointerdown=event=>{screen.focus();screen.setPointerCapture(event.pointerId);const r=screen.getBoundingClientRect();const x=Math.floor((event.clientX-r.left)*screen.width/r.width),y=Math.floor((event.clientY-r.top)*screen.height/r.height);send(0x201,1,(x&65535)|((y&65535)<<16));};
const keys={Escape:27,Enter:13,ArrowLeft:37,ArrowUp:38,ArrowRight:39,ArrowDown:40,Shift:16,Control:17,Alt:18,' ':32};
function keycode(event){return keys[event.key]??(/^[a-z0-9]$/i.test(event.key)?event.key.toUpperCase().charCodeAt(0):null);}
screen.onkeydown=event=>{const key=keycode(event);if(key!==null){event.preventDefault();send(0x100,key);}};
screen.onkeyup=event=>{const key=keycode(event);if(key!==null){event.preventDefault();send(0x101,key);}};
