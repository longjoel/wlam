import assert from 'node:assert/strict';
import {Worker} from 'node:worker_threads';
import {BUFFER_BYTES,enqueue} from '../queue.mjs';
const input=new SharedArrayBuffer(BUFFER_BYTES);
const worker=new Worker(new URL('./guest-worker.mjs',import.meta.url),{workerData:{input}});
const frames=[];let phase=0;
const timer=setTimeout(()=>{worker.terminate();console.error('Guest worker timeout');process.exitCode=1;},20000);
try {
  await new Promise((resolve,reject)=>{
    worker.on('error',reject);
    worker.on('message',message=>{
      try{
        if(message.type==='error')throw Error(message.message);
        if(message.type==='frame')frames.push(message);
        if(message.state==='waiting'){
          const [event,wparam]=[[0x201,1],[0x100,65],[0x100,27]][phase++];
          enqueue(input,{window:message.window,message:event,wparam});
        }
        if(message.state==='exited'){assert.equal(message.code,0);resolve();}
      }catch(error){reject(error);}
    });
    worker.on('exit',code=>{if(code)reject(Error(`worker exit ${code}`));});
  });
  assert.equal(frames.length,3);
  for(const [index,pixel] of [[0,[0x12,0x34,0x56,255]],[1,[0xe0,0x40,0x20,255]],[2,[0x20,0xc0,0x60,255]]]){
    const frame=frames[index];assert.equal(frame.width,32);assert.equal(frame.height,24);
    for(let at=0;at<frame.pixels.length;at+=4)assert.deepEqual([...frame.pixels.slice(at,at+4)],pixel);
  }
  console.log('Actual PE + Rust host Wasm: worker input, exact frames and clean exit passed');
}finally{clearTimeout(timer);await worker.terminate();}
