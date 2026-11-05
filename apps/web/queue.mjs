// Single producer (page) / single consumer (guest worker). Publish only after
// writing a complete record. Never silently drop input when the queue is full.
export const CAPACITY=256;
export const BUFFER_BYTES=(4+CAPACITY*4)*4;
// Internal host event. It is consumed by the worker and never posted to USER32.
export const RESIZE_GUEST_WINDOW=0xffff_fffe;
export function enqueue(buffer,{window,message,wparam=0,lparam=0}) {
  const q=new Int32Array(buffer), read=Atomics.load(q,1)>>>0, write=Atomics.load(q,2)>>>0;
  if((write-read)>>>0>=CAPACITY)throw Error('Guest input queue is full');
  const at=4+(write%CAPACITY)*4;
  q[at]=window;q[at+1]=message;q[at+2]=wparam;q[at+3]=lparam;
  Atomics.store(q,2,(write+1)|0);Atomics.add(q,0,1);Atomics.notify(q,0);
}
export function dequeue(buffer) {
  const q=new Int32Array(buffer),read=Atomics.load(q,1)>>>0;
  if(read===(Atomics.load(q,2)>>>0))return null;
  const at=4+(read%CAPACITY)*4;
  const result={window:q[at]>>>0,message:q[at+1]>>>0,wparam:q[at+2]>>>0,lparam:q[at+3]>>>0};
  Atomics.store(q,1,(read+1)|0);return result;
}
export function waitForInput(buffer,timeout=Infinity) {
  const q=new Int32Array(buffer);
  const end=Number.isFinite(timeout)?Date.now()+timeout:Infinity;
  for(;;){const seq=Atomics.load(q,0);const event=dequeue(buffer);if(event)return event;const remaining=end-Date.now();if(remaining<=0)return null;if(Atomics.wait(q,0,seq,remaining)==='timed-out')return null;}
}
