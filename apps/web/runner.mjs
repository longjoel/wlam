import {dequeue,waitForInput,RESIZE_GUEST_WINDOW} from './queue.mjs';
import {dialogButtons,DIALOG_RESPONSE} from './dialog.mjs';
export function runBundle({manifest,moduleBytes,imageBytes,hostBytes,files=[],input,onFrame,onGpuFrame,gpuOnly=false,onStatus,onTrace,onDialog,onCallback,onApi}) {
  if(manifest.version!==1)throw Error('Unsupported bundle version');
  let guest,host,window=0,depth=0;
  const deferredInput=[];
  const image=new Uint8Array(imageBytes);
  const decode=(p,n)=>new TextDecoder().decode(new Uint8Array(host.memory.buffer,p,n));
  const error=()=>Error(decode(host.error_text(0),host.error_text(1)));
  function copy(address,pointer,length,write){
    address>>>=0;pointer>>>=0;length>>>=0;
    if(!guest || address+length>guest.memory.buffer.byteLength || pointer+length>host.memory.buffer.byteLength)return 0;
    const g=new Uint8Array(guest.memory.buffer,address,length),h=new Uint8Array(host.memory.buffer,pointer,length);
    if(write)g.set(h);else h.set(g);return 1;
  }
  host=new WebAssembly.Instance(new WebAssembly.Module(hostBytes),{guest:{
    read_guest:(a,p,n)=>copy(a,p,n,false),write_guest:(a,p,n)=>copy(a,p,n,true),guest_length:()=>BigInt(guest.memory.buffer.byteLength),
  }}).exports;
  host.initialize(manifest.imageBase);
  if(gpuOnly&&host.set_gpu_mode)host.set_gpu_mode(1);
  else if(onGpuFrame&&host.set_gpu_capture)host.set_gpu_capture(1);
  const apis=new Map();
  for(let i=0;i<host.api_count();i++){
    const dll=decode(host.api_text(i,0,0),host.api_text(i,0,1)).toLowerCase();
    const name=decode(host.api_text(i,1,0),host.api_text(i,1,1));apis.set(`${dll}!${name}`,i);
  }
  const exit={};let exitCode=null;
  function present(){
    if(onGpuFrame&&host.gpu_frame&&host.gpu_frame_length){
      const pointer=host.gpu_frame(window);
      const byteLength=host.gpu_frame_length();
      if(pointer&&byteLength){
        const words=new Uint32Array(host.memory.buffer,pointer,byteLength/4).slice();
        window=host.gpu_window?.()||window;
        onGpuFrame({window,words});
      }
    }
    if(gpuOnly)return;
    if(!window)return;
    const pointer=host.frame(window);if(!pointer)return;
    const width=host.frame_width(),height=host.frame_height();
    const pixels=new Uint8ClampedArray(host.memory.buffer,pointer,width*height*4).slice();
    onFrame({window,width,height,pixels});
  }
  function drainInput(){
    if(!input)return;
    for(let event;(event=dequeue(input));){
      if(event.message===RESIZE_GUEST_WINDOW){
        if(host.resize_window(event.window,event.wparam,event.lparam)!==1)throw error();
      }else if(host.post(event.window,event.message,event.wparam,event.lparam)===4)throw error();
    }
  }
  function invoke(index,argsPointer,name){
    let kind=host.dispatch(index,argsPointer);
    for(;;){
      if(kind===0){if(name==='EndPaint'||name==='SwapBuffers')present();return host.action_value()|0;}
      if(kind===4){
        const view=new DataView(guest.memory.buffer),at=argsPointer>>>0;
        const words=Array.from({length:6},(_,i)=>at+i*4+4<=view.byteLength?'0x'+view.getUint32(at+i*4,true).toString(16):'?');
        throw Error(`${name}: ${error().message} (arguments: ${words.join(', ')})`);
      }
      if(kind===3){exitCode=host.action_value();throw exit;}
      if(kind===5){
        if(!onDialog)throw Error('MessageBoxA requires a dialog presenter');
        const text=field=>new TextDecoder('windows-1252').decode(new Uint8Array(host.memory.buffer,host.dialog_text(field,0),host.dialog_text(field,1)));
        const flags=host.action_value();
        let result=onDialog({text:text(0),title:text(1),flags});
        while(result===undefined){
          const event=waitForInput(input);
          if(event.message===DIALOG_RESPONSE)result=event.wparam;
          else deferredInput.push(event);
        }
        if(!dialogButtons(flags)?.some(([id])=>id===result))throw Error('Invalid dialog response');
        return result;
      }
      if(kind===1){
        const token=host.action_token(),procedure=host.action_procedure();
        const args=Array.from({length:host.action_argument_count?.()??4},(_,i)=>host.action_argument(i));
        if(args.length===4)window=args[0];
        if(++depth>64)throw Error('Guest callback nesting exceeds 64');
        const vector=0x20000+depth*256;
        const data=new DataView(guest.memory.buffer);args.forEach((v,i)=>data.setUint32(vector+i*4,v,true));
        onCallback?.({procedure,args});
        let result;try{result=guest.invoke_guest(procedure,args.length,vector);}finally{depth--;}
        onCallback?.({procedure,result});
        kind=host.resume(token,result);continue;
      }
      if(kind===2){
        // The WebGL canvas retains the last completed swap. Emitting an empty
        // packet while GetMessage blocks would count and pace it as a new
        // animation frame. Keep the redraw behavior for CPU-backed canvases.
        if(!gpuOnly)present();
        onStatus({state:'waiting',window});
        const event=deferredInput.shift()??waitForInput(input,host.wait_timeout?.()??Infinity);
        if(event){
          if(event.message===RESIZE_GUEST_WINDOW){
            if(host.resize_window(event.window,event.wparam,event.lparam)!==1)throw error();
          }else if(host.post(event.window,event.message,event.wparam,event.lparam)===4)throw error();
        }
        else if(host.advance_time?.(host.wait_timeout())!==1)throw error();
        kind=host.dispatch(index,argsPointer);continue;
      }
      throw Error(`Invalid host action ${kind}`);
    }
  }
  const imports={};
  for(const entry of manifest.imports){
    const index=apis.get(`${entry.library.toLowerCase()}!${entry.name}`);
    if(index!==undefined&&host.register_import)for(const slot of entry.slots??[])host.register_import(index,slot);
    (imports[entry.library]??={})[entry.name]=(args,_slot)=>{
      // PeekMessage-based games can keep executing guest code without ever
      // entering the blocking GetMessage path. Drain page input at each host
      // boundary so those loops still observe keyboard and mouse events.
      drainInput();
      if(entry.library==='wlam.trace'){
        const registers={};const traceView=new DataView(guest.memory.buffer);
        for(const name of ['EIP','EAX','EBX','ECX','EDX','ESI','EDI','EBP','ESP']){const entry=manifest.registers.find(r=>r.name===name);if(entry)registers[name]=traceView.getUint32(entry.address,true);}
        const source=registers.ESI??0,sourceWords=source+64<=traceView.byteLength?Array.from({length:16},(_,i)=>traceView.getUint32(source+i*4,true)):[];
        const esp=registers.ESP??0,stackWords=esp+32<=traceView.byteLength?Array.from({length:8},(_,i)=>traceView.getUint32(esp+i*4,true)):[];
        const ebp=registers.EBP??0,frameStart=Math.max(0,ebp-0x100),frameEnd=Math.min(traceView.byteLength,ebp+0x20),frameWords=frameEnd>=frameStart+4?Array.from({length:(frameEnd-frameStart)/4},(_,i)=>traceView.getUint32(frameStart+i*4,true)):[];
        const object=registers.EBX??0,objectWords=object&&object+96<=traceView.byteLength?Array.from({length:24},(_,i)=>traceView.getUint32(object+i*4,true)):[];
        const wordsAt=pointer=>pointer&&pointer+96<=traceView.byteLength?Array.from({length:24},(_,i)=>traceView.getUint32(pointer+i*4,true)):[];
        const key=stackWords[1]??0,keyEnd=key<traceView.byteLength?new Uint8Array(traceView.buffer,key,Math.min(128,traceView.byteLength-key)).indexOf(0):-1;
        const mapWords=0x4a4768+32<=traceView.byteLength?Array.from({length:8},(_,i)=>traceView.getUint32(0x4a4768+i*4,true)):[];
        const mapTable=mapWords[1]&&mapWords[1]+mapWords[2]*4<=traceView.byteLength?Array.from({length:mapWords[2]},(_,i)=>traceView.getUint32(mapWords[1]+i*4,true)):[];
        const node=mapTable.find(p=>p!==0)??0,nodeWords=node&&node+28<=traceView.byteLength?Array.from({length:7},(_,i)=>traceView.getUint32(node+i*4,true)):[];
        const nodeKey=nodeWords[2]??0,nodeEnd=nodeKey<traceView.byteLength?new Uint8Array(traceView.buffer,nodeKey,Math.min(128,traceView.byteLength-nodeKey)).indexOf(0):-1;
        onTrace?.({address:args>>>0,value:traceView.getUint32(args>>>0,true),registers,sourceWords,stackWords,frameStart,frameWords,objectWords,ecxWords:wordsAt(registers.ECX),ediWords:wordsAt(registers.EDI),keyString:keyEnd>=0?new TextDecoder().decode(new Uint8Array(traceView.buffer,key,keyEnd)):null,mapWords,mapTable,nodeWords,nodeKeyString:nodeEnd>=0?new TextDecoder().decode(new Uint8Array(traceView.buffer,nodeKey,nodeEnd)):null,indirect:{target:traceView.getUint32(0x7fe0,true),site:traceView.getUint32(0x7fe4,true),receiver:traceView.getUint32(0x7fe8,true)},stack:new Error().stack});
        return 0;
      }
      if(index===undefined){
        const view=new DataView(guest.memory.buffer),at=args>>>0;
        const words=Array.from({length:6},(_,i)=>at+i*4+4<=view.byteLength?view.getUint32(at+i*4,true):0);
        const strings=words.map(pointer=>{
          if(!pointer||pointer>=view.byteLength)return '';
          const bytes=new Uint8Array(view.buffer,pointer,Math.min(1024,view.byteLength-pointer)),end=bytes.indexOf(0);
          return end>0&&bytes.slice(0,end).every(b=>b===9||b===10||b===13||b>=32&&b<127)?new TextDecoder().decode(bytes.slice(0,end)):'';
        });
        throw Error(`Unsupported import ${entry.library}!${entry.name} (arguments at 0x${at.toString(16)}: ${words.map(w=>'0x'+w.toString(16)).join(', ')}; strings: ${JSON.stringify(strings)})`);
      }
      const count=Math.max(0,((entry.returnPop??4)-4)/4);
      const view=new DataView(guest.memory.buffer),at=args>>>0;
      const api={library:entry.library,name:entry.name,args:Array.from({length:count},(_,i)=>view.getUint32(at+i*4,true)),returnAddress:at>=4?view.getUint32(at-4,true):0,registers:Object.fromEntries(['EIP','ESP','EBP','ESI','EDI'].flatMap(name=>{const r=manifest.registers.find(r=>r.name===name);return r?[[name,view.getUint32(r.address,true)]]:[]}))};
      if(entry.name==='OpenFile'||entry.name==='CreateFileA') {const p=api.args[0],end=new Uint8Array(view.buffer,p,Math.min(260,view.byteLength-p)).indexOf(0);api.path=new TextDecoder().decode(new Uint8Array(view.buffer,p,end<0?260:end));}
      const result=invoke(index,args>>>0,entry.name);
      // The optional second argument is available to in-process diagnostics
      // that need to decode pointer-backed API parameters. Existing callbacks
      // consume the event object only, so browser worker messages stay small.
      onApi?.({...api,result},guest.memory);
      return result;
    };
  }
  guest=new WebAssembly.Instance(new WebAssembly.Module(moduleBytes),imports).exports;
  for(const segment of manifest.segments){
    if(segment.offset+segment.length>image.length || segment.address+segment.mappedSize>guest.memory.buffer.byteLength || segment.length>segment.mappedSize)throw Error('Invalid image segment');
    new Uint8Array(guest.memory.buffer,segment.address,segment.mappedSize).fill(0);
    new Uint8Array(guest.memory.buffer,segment.address,segment.length).set(image.subarray(segment.offset,segment.offset+segment.length));
    if(segment.executable&&host.register_code)host.register_code(segment.address,segment.mappedSize);
  }
  for(const resource of manifest.resources??[]){
    if(typeof resource.name==='number'&&host.register_resource){
      if(!host.register_resource(resource.type,resource.name,resource.language,resource.address,resource.size))throw error();
    }
  }
  for(const entry of manifest.imports)for(const slot of entry.slots??[]){
    if(!manifest.segments.some(segment=>slot>=segment.address&&slot+4<=segment.address+segment.mappedSize))throw Error('IAT slot outside mapped image');
    new DataView(guest.memory.buffer).setUint32(slot,slot,true);
  }
  if(files.length){
    const encoded=files.map(file=>({path:new TextEncoder().encode(`${file.path}\0`),bytes:new Uint8Array(file.bytes)}));
    const required=encoded.reduce((sum,file)=>sum+file.path.length+file.bytes.length,0);
    // Guest modules reserve the complete 32-bit address space up front, so
    // stage files in the free gap after the image and before VirtualAlloc's
    // reserved arena at 0x08000000.
    let cursor=Math.ceil(Math.max(...manifest.segments.map(segment=>segment.address+segment.mappedSize))/65536)*65536;
    if(cursor+required>0x08000000)throw Error('Hover data assets exceed the guest staging region');
    for(const file of encoded){
      const path=cursor;new Uint8Array(guest.memory.buffer,cursor,file.path.length).set(file.path);cursor+=file.path.length;
      const data=cursor;new Uint8Array(guest.memory.buffer,cursor,file.bytes.length).set(file.bytes);cursor+=file.bytes.length;
      if(host.register_file(path,data,file.bytes.length)!==1)throw error();
    }
  }
  const data=new DataView(guest.memory.buffer);
  for(const register of manifest.registers){let value=0;if(register.name==='ESP')value=manifest.stack;if(register.name==='FS_OFFSET')value=manifest.tib;data.setUint32(register.address,value,true);}
  data.setUint32(manifest.tib,0xffffffff,true);
  onStatus({state:'running'});
  try{guest.entry();}catch(cause){
    if(cause!==exit){
      const view=new DataView(guest.memory.buffer);
      const registers=manifest.registers.filter(r=>/^E(AX|BX|CX|DX|SI|DI|BP|SP|IP)$/.test(r.name));
      const state=registers.map(r=>`${r.name}=0x${view.getUint32(r.address,true).toString(16)}`).join(' ');
      const eaxValue=view.getUint32(manifest.registers.find(r=>r.name==='EAX').address,true),eaxSlot=eaxValue+0x5c;
      const callSlot=eaxSlot+4<=view.byteLength?`MFC call slot: [0x${eaxSlot.toString(16)}]=0x${view.getUint32(eaxSlot,true).toString(16)}`:'';
      const ecxValue=view.getUint32(manifest.registers.find(r=>r.name==='ECX').address,true),objectMemory=ecxValue>=16&&ecxValue+32<=view.byteLength?`MFC object words: ${Array.from({length:12},(_,i)=>view.getUint32(ecxValue-16+i*4,true).toString(16)).join(' ')}`:'';
      const listNode=0x8009730,listNodeMemory=listNode+160<=view.byteLength?`Dispatch list node @0x${listNode.toString(16)}: ${Array.from({length:40},(_,i)=>view.getUint32(listNode+i*4,true).toString(16)).join(' ')}`:'';
      const watch=`Store watch: address=0x${view.getUint32(0x7fb0,true).toString(16)} block=0x${view.getUint32(0x7fb4,true).toString(16)}`;
      const indirectSite=view.getUint32(0x7ff4,true),indirect=indirectSite?`Last indirect transfer: target=0x${view.getUint32(0x7ff0,true).toString(16)} site=0x${indirectSite.toString(16)} function=0x${view.getUint32(0x7ff8,true).toString(16)}`:'';
      const dispatch=`Dispatch snapshot: eip=0x${view.getUint32(0x7fc0,true).toString(16)} eip+base=0x${view.getUint32(0x7fc4,true).toString(16)} no-match=${view.getUint32(0x7fc8,true)}`;
      const objects=registers.filter(r=>['ECX','ESI','ESP'].includes(r.name)).map(r=>{
        const pointer=view.getUint32(r.address,true);
        return pointer&&pointer+32<=view.byteLength?`${r.name}[${Array.from({length:8},(_,i)=>'0x'+view.getUint32(pointer+i*4,true).toString(16)).join(',')}]`:'';
      }).join(' ');
      const ebp=view.getUint32(manifest.registers.find(r=>r.name==='EBP').address,true),frameStart=Math.max(0,ebp-0x40),frameEnd=Math.min(view.byteLength,ebp+0x40);
      const frame=`EBP frame 0x${frameStart.toString(16)}: ${Array.from({length:(frameEnd-frameStart)/4},(_,i)=>view.getUint32(frameStart+i*4,true).toString(16)).join(' ')}`;
      const pointerWords=offset=>{const slot=ebp+offset;if(slot<0||slot+4>view.byteLength)return '';const p=view.getUint32(slot,true);return p+128<=view.byteLength?`[EBP${offset}]=0x${p.toString(16)}:${Array.from({length:32},(_,i)=>view.getUint32(p+i*4,true).toString(16)).join(' ')}`:'';};
      throw Error(`${cause.stack??cause.message}\nGuest state: ${state}${indirect?`\n${indirect}`:''}\n${dispatch}\n${watch}\n${callSlot}\n${objectMemory}\n${listNodeMemory}\nGuest memory: ${objects}\n${frame}\n${pointerWords(-0x18)}\n${pointerWords(-0x14)}`,{cause});
    }
  }
  onStatus({state:'exited',code:exitCode});
  return {exitCode};
}
