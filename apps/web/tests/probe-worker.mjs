// Run the same guest and Rust browser host in a bounded diagnostic worker.
import {Worker} from 'node:worker_threads';
import {BUFFER_BYTES} from '../queue.mjs';
const bundle=process.argv[2]??'target/hover/bundle/';
const worker=new Worker(new URL('./guest-worker.mjs',import.meta.url),{
  workerData:{bundle:bundle.endsWith('/')?bundle:bundle+'/',input:new SharedArrayBuffer(BUFFER_BYTES)},
});
const timer=setTimeout(()=>{console.log('TIMEOUT');worker.terminate();process.exitCode=1;},30000);
const recentApis=[];
const recentTraces=[];
worker.on('message',async message=>{
  if(message.type==='api'){recentApis.push(message);if(recentApis.length>60)recentApis.shift();if(message.name.startsWith('Tls'))console.log('TLS_API',JSON.stringify(message));if(['VirtualAlloc','VirtualFree','VirtualProtect'].includes(message.name))console.log('VM_API',JSON.stringify(message));if(['CreateFileA','ReadFile','SetFilePointer'].includes(message.name))console.log('FILE_API',JSON.stringify(message));if(['CreateWindowExA','SetWindowLongA','GetWindowLongA','DestroyWindow','GetCursorPos','GetTextExtentPointA'].includes(message.name))console.log('WINDOW_API',JSON.stringify(message));}
  else if(message.type==='trace'&&[0x44c438,0x44c461,0x44c46a,0x44a384,0x44a3b8,0x44a3e5,0x44a30b,0x44a225,0x44b050,0x44b0a1,0x44b0c0].includes(message.address))console.log('HANDLE_MAP_TRACE',JSON.stringify({address:message.address,registers:message.registers,stack:message.stackWords,frame:message.frameWords,object:message.objectWords,ecx:message.ecxWords,edi:message.ediWords}));
  else if(message.type==='trace'&&[0x44060c,0x445078].includes(message.address))console.log('STRING_COPY_TRACE',JSON.stringify({address:message.address,registers:message.registers,stack:message.stackWords,ecx:message.ecxWords,edi:message.ediWords}));
  else if(message.type==='trace'&&[0x4431bb,0x44327b,0x443286,0x443294,0x44329b,0x4432a3,0x44389f].includes(message.address))console.log('CRT_PARSE_TRACE',JSON.stringify({address:message.address,registers:message.registers,stack:message.stackWords,frameStart:message.frameStart,frame:message.frameWords,ecx:message.ecxWords,edi:message.ediWords}));
  else if(message.type==='trace'&&[0x4b7de0,0x4b7de8].includes(message.address))console.log('THREAD_STATE_STORE',JSON.stringify({address:message.address,value:message.value,registers:message.registers,stack:message.stackWords,frame:message.frameWords,ecx:message.ecxWords,edi:message.ediWords}));
  else if(message.type==='trace'&&message.address===0x7fe0&&message.value>=0x45e000&&message.value<0x45e100)console.log('CALL_IN_45E_RANGE',JSON.stringify({target:message.value,site:message.indirect?.site,receiver:message.indirect?.receiver,registers:message.registers,stack:message.stackWords,frame:message.frameWords}));
  else if(message.type==='trace'&&[0x458c93,0x458ca6,0x45e019,0x45ea69,0x45ea8b,0x441189,0x4410ac,0x4406cc].includes(message.address))console.log('OBJECT_LIFETIME_TRACE',JSON.stringify({address:message.address,registers:message.registers,stack:message.stackWords,frame:message.frameWords,ecx:message.ecxWords,edi:message.ediWords}));
  else if(message.type==='trace'&&[0x4406cc,0x4410ac,0x4410f5,0x441189,0x4411a8].includes(message.address)){const r=message.registers??{},s=message.stackWords??[];if([r.EAX,r.EBX,r.ESI,r.EDI,...s].some(v=>v>=0x8009000&&v<0x800a000))console.log('BLOCK_LIFETIME_TRACE',JSON.stringify({address:message.address,registers:r,stack:s,frame:message.frameWords}));}
  else if(message.type==='trace'&&[0x442bdf,0x442cba,0x442cbf,0x442ccf,0x442d2a,0x442d35,0x442db5,0x442db9].includes(message.address)){const r=message.registers??{},s=message.stackWords??[];if(message.address===0x442bdf?s[1]>=0x80&&s[1]<=0x100:(r.EAX>=0x8009000&&r.EAX<0x800a000)||(r.ESI>=0x8009000&&r.ESI<0x800a000)||s.includes(0x8009730))console.log('HEAP_ALLOC_TRACE',JSON.stringify({address:message.address,registers:r,stack:s,frame:message.frameWords}));}
  else if(message.type==='trace'&&((message.address>=0x80098f0&&message.address<0x8009920)||[0x8008f90,0x800977c,0x8009730].includes(message.address)))console.log('OBJECT_LIST_STORE',JSON.stringify({address:message.address,value:message.value,registers:message.registers,stack:message.stackWords,frame:message.frameWords,ecx:message.ecxWords,edi:message.ediWords}));
  else if(message.type==='trace'&&[0x44b03a,0x44b050,0x44b07f,0x44b0c0].includes(message.address))console.log('MFC_CALL_TRACE',JSON.stringify({address:message.address,registers:message.registers,stack:message.stackWords,frame:message.frameWords,object:message.objectWords}));
  else if(message.type==='trace'&&[0x7fd0,0x7fd4,0x7fd8,0x7fdc,0x7fe0,0x7fe4,0x7fe8,0x7fec,0x7fee,0x7fef,0x7ff0,0x7ff1,0x7ff2,0x7ff3,0x7ff4,0x7ff8,0x8040,0x417c38,0x417c49,0x417c8f,0x417c92,0x417c9c,0x417caa,0x417cb6,0x417ccc,0x40fe40,0x40fe59,0x40fe5c,0x40fe64,0x40fe68,0x449f18,0x449f34,0x449f39,0x449f3f,0x449f42,0x449f46,0x449f49,0x449f4d,0x449f5b,0x449f63,0x449f6a,0x449f6e,0x449f7f,0x449f87,0x449f90,0x43e896,0x43e8b2,0x43e8b9,0x417d6e,0x424a70,0x424b0c,0x424b10,0x453f02,0x453f3a,0x453f5b,0x453f60,0x453d42,0x41d31f,0x41d324,0x41d329,0x41d32e,0x41d333,0x41d338,0x41d343,0x41d353,0x41d35e,0x41d364].includes(message.address)){
    const r=message.registers??{};const i=message.indirect??{};
    if([0x417c38,0x417c49,0x417c8f,0x417c92,0x417c9c,0x417caa,0x417cb6,0x417ccc,0x40fe40,0x40fe59,0x40fe5c,0x40fe64,0x40fe68,0x449f18,0x449f34,0x449f39,0x449f3f,0x449f42,0x449f46,0x449f49,0x449f4d,0x449f5b,0x449f63,0x449f6a,0x449f6e,0x449f7f,0x449f87,0x449f90,0x43e896,0x43e8b2,0x43e8b9,0x417d6e].includes(message.address))console.log('PARSER_TRACE',JSON.stringify({address:message.address,registers:r,stack:message.stackWords,frame:message.frameWords,keyString:message.keyString,mapWords:message.mapWords,mapTable:message.mapTable,nodeWords:message.nodeWords,nodeKeyString:message.nodeKeyString}));
    if([0x7fd0,0x7fd4].includes(message.address)&&r.EIP===0x449f34)console.log('HASH_DIV',JSON.stringify({address:message.address,value:message.value,registers:r,stack:message.stackWords}));
    recentTraces.push({address:message.address,value:message.value,target:i.target,site:i.site,receiver:i.receiver,eip:r.EIP,ecx:r.ECX,esi:r.ESI,ebp:r.EBP,esp:r.ESP,stack:message.stackWords,...([0x453f02,0x41d35e,0x41d364].includes(message.address)||message.address>=0x7fd0&&message.address<=0x7fec?{frameStart:message.frameStart,frame:message.frameWords}: {})});if(recentTraces.length>60)recentTraces.shift();
  }
  else if(message.type==='trace'&&message.address===0x6fe70){
    const r=message.registers??{};recentTraces.push({field:message.address,value:message.value,eip:r.EIP,eax:r.EAX,ecx:r.ECX,esi:r.ESI,ebp:r.EBP,esp:r.ESP});if(recentTraces.length>60)recentTraces.shift();
  }
  else if(message.type==='trace'&&message.address===0x6fe64){
    const r=message.registers??{};recentTraces.push({field:message.address,value:message.value,eip:r.EIP,esi:r.ESI,ebp:r.EBP});if(recentTraces.length>60)recentTraces.shift();
  }
  else if(message.type==='frame') console.log('FRAME',message.width,message.height);
  else if(message.type==='callback') console.log('CALLBACK',JSON.stringify(message));
  else if(message.type!=='trace') console.log(JSON.stringify(message));
  if(message.type==='error'||message.state==='waiting'||message.state==='exited'){
    if(message.type==='error')for(const api of recentApis)console.log('RECENT_API',JSON.stringify(api));
    if(message.type==='error')for(const trace of recentTraces)console.log('RECENT_TRACE',JSON.stringify(trace));
    if(message.type==='error')process.exitCode=1;
    clearTimeout(timer);await worker.terminate();
  }
});
worker.on('error',error=>{clearTimeout(timer);console.error(error);process.exitCode=1;});
worker.on('exit',()=>clearTimeout(timer));
