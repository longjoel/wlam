import assert from 'node:assert/strict';
import {Worker} from 'node:worker_threads';
import {BUFFER_BYTES,enqueue} from '../queue.mjs';
const input=new SharedArrayBuffer(BUFFER_BYTES);
const worker=new Worker(new URL('./guest-worker.mjs',import.meta.url),{workerData:{input}});
const frames=[];let phase=0,pendingInput=null;
const timer=setTimeout(()=>{worker.terminate();console.error('Guest worker timeout');process.exitCode=1;},20000);
try {
  await new Promise((resolve,reject)=>{
    worker.on('error',reject);
    worker.on('message',message=>{
      try{
        if(message.type==='error')throw Error(message.message);
        if(message.type==='frame')frames.push(message);
        if(message.state==='waiting'){
          assert.equal(pendingInput,null,'An indefinite guest wait must block until input, without repeated waiting notifications');
          const [event,wparam]=[[0x201,1],[0x100,65],[0x100,27]][phase++];
          // Leave the real PE worker idle long enough to expose signed -1
          // timeout spin loops before delivering the next input event.
          pendingInput=setTimeout(()=>{pendingInput=null;enqueue(input,{window:message.window,message:event,wparam});},100);
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
}finally{clearTimeout(timer);clearTimeout(pendingInput);await worker.terminate();}
