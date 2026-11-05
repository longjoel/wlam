// Execute the same PE fixture through the browser host, collecting its stdout.
import assert from 'node:assert/strict';
import {readFileSync,writeFileSync} from 'node:fs';
import {runBundle} from '../apps/web/runner.mjs';
const directory=process.argv[2]??'target/gdi/bundle';
const manifest=JSON.parse(readFileSync(`${directory}/manifest.json`));
let output='';
let exitCode;
try{({exitCode}=runBundle({manifest,
  moduleBytes:readFileSync(`${directory}/guest.wasm`),imageBytes:readFileSync(`${directory}/image.bin`),
  hostBytes:readFileSync('target/wasm32-unknown-unknown/debug/wlam_browser_host.wasm'),
  onFrame:()=>{},onStatus:()=>{},
  onApi:(api,memory)=>{if(api.name==='WriteFile'&&api.result===1)output+=new TextDecoder().decode(new Uint8Array(memory.buffer,api.args[1],api.args[2]));},
}));}finally{writeFileSync('target/gdi/observed.jsonl',output);}
const records=output.trim().split('\n').filter(Boolean).map(line=>JSON.parse(line));
assert.ok(records.length>=30,'Fixture must emit all observations');
for(const record of records)assert.equal(record.value,record.expected,record.case);
assert.equal(exitCode,0,'Guest fixture exit code');
if(process.argv[3]){
 const reference=readFileSync(process.argv[3],'utf8').trim().split(/\r?\n/).map(line=>JSON.parse(line));
 assert.deepEqual(records.map(({case:name,value})=>({case:name,value})),reference.map(({case:name,value})=>({case:name,value})),'Windows/reference observations differ');
 console.log(`GDI: ${records.length} observations match the supplied reference`);
}else console.log(`GDI: ${records.length} local expectations passed; no Windows reference supplied`);
