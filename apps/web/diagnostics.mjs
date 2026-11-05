// Bounded guest snapshots. Frame chains are evidence, not a verified unwind:
// optimized x86 code may use EBP as an ordinary register.
export function guestSnapshot(memory,registerDefinitions){
  const view=new DataView(memory.buffer);
  const word=address=>Number.isInteger(address)&&address>=0&&address+4<=view.byteLength?view.getUint32(address,true):null;
  const registers=Object.fromEntries(registerDefinitions.filter(r=>/^E(AX|BX|CX|DX|SI|DI|BP|SP|IP)$/.test(r.name)).map(r=>[r.name,word(r.address)]));
  const stack=[];
  if(registers.ESP!==null&&registers.ESP!==undefined)for(let i=0;i<32;i++){
    const address=registers.ESP+i*4,value=word(address);if(value===null)break;stack.push({address,value});
  }
  const frames=[],seen=new Set();let pointer=registers.EBP;
  let frameChainEnd='limit';
  for(let i=0;i<16;i++){
    if(!pointer){frameChainEnd='end';break;}
    if(seen.has(pointer)){frameChainEnd='cycle';break;}
    seen.add(pointer);
    const previous=word(pointer),returnAddress=word(pointer+4);
    if(previous===null||returnAddress===null){frameChainEnd='outside-memory';break;}
    frames.push({address:pointer,previous,returnAddress});
    if(previous&&previous<=pointer){frameChainEnd='non-increasing';break;}
    pointer=previous;
  }
  return {registers,stack,frames,frameChainEnd};
}
export function formatDebugReport(report){
  return JSON.stringify(report,null,2)+'\n';
}
