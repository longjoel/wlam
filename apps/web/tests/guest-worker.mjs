import {parentPort,workerData} from 'node:worker_threads';
import {readFileSync} from 'node:fs';
import {runBundle} from '../runner.mjs';
import {dialogButtons} from '../dialog.mjs';
try {
  const path=workerData.bundle??'target/interactive/bundle/';
  const manifest=JSON.parse(readFileSync(path+'manifest.json'));
  const files=(manifest.files??[]).map(file=>({path:file.path,bytes:readFileSync(path+file.url)}));
  runBundle({manifest,moduleBytes:readFileSync(path+'guest.wasm'),imageBytes:readFileSync(path+'image.bin'),hostBytes:readFileSync('target/wasm32-unknown-unknown/debug/wlam_browser_host.wasm'),files,input:workerData.input,
    onFrame:frame=>parentPort.postMessage({type:'frame',...frame}),onStatus:status=>parentPort.postMessage({type:'status',...status}),
    onTrace:trace=>parentPort.postMessage({type:'trace',...trace}),
    onApi:api=>parentPort.postMessage({type:'api',...api}),
    onCallback:callback=>parentPort.postMessage({type:'callback',...callback}),
    onDialog:dialog=>{
      const buttons=dialogButtons(dialog.flags);
      if(buttons?.length!==1||buttons[0][0]!==1)throw Error('Diagnostic worker only acknowledges OK-only dialogs');
      parentPort.postMessage({type:'dialog',...dialog,response:1,automatic:true});return 1;
    }});
}catch(error){parentPort.postMessage({type:'error',message:error.stack});}
