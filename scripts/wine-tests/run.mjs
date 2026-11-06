// Run one compiled Wine conformance test through the browser host.
import {readFileSync} from 'node:fs';
import {runBundle} from '../../apps/web/runner.mjs';

const directory = process.argv[2];
if (!directory) throw Error('usage: run.mjs <bundle-directory>');
const manifest = JSON.parse(readFileSync(`${directory}/manifest.json`));
let output = '';
const standardHandles = new Set();
let failure = null;
let exitCode = null;
try {
  ({exitCode} = runBundle({
    manifest,
    moduleBytes: readFileSync(`${directory}/guest.wasm`),
    imageBytes: readFileSync(`${directory}/image.bin`),
    hostBytes: readFileSync('target/wasm32-unknown-unknown/debug/wlam_browser_host.wasm'),
    onStatus: () => {},
    onApi: (api, memory) => {
      if (api.name === 'GetStdHandle' && api.result) {
        standardHandles.add(api.result >>> 0);
      }
      if (api.name === 'WriteFile' && api.result === 1 && standardHandles.has(api.args[0] >>> 0)) {
        output += new TextDecoder().decode(new Uint8Array(memory.buffer, api.args[1], api.args[2]));
      }
      // WLAM_API_LOG=1 prints every host call; the last one before a trap names
      // the guest path that failed.
      if (process.env.WLAM_API_LOG) {
        console.error(`${api.library}!${api.name}(${api.args.map(value => '0x' + value.toString(16)).join(', ')}) = 0x${(api.result >>> 0).toString(16)}`);
      }
    },
  }));
} catch (error) {
  failure = error;
}
process.stdout.write(output);
const result = /^RESULT (\d+) (\d+) (\d+)$/m.exec(output);
if (!result) {
  if (failure) console.error(`\nHARNESS ERROR: ${failure.message ?? failure}`);
  else console.error('\nHARNESS ERROR: test did not emit a RESULT line');
  process.exitCode = 1;
} else {
  if (failure) console.error(`\nTrailing harness error after RESULT: ${failure.message ?? failure}`);
  const [, passes, failures, skips] = result.map(Number);
  console.log(`\nWine ${manifest.name ?? 'test'}: ${passes} passed, ${failures} failed, ${skips} skipped (guest exit ${exitCode})`);
  process.exitCode = failures || (exitCode !== null && exitCode !== 0) ? 1 : 0;
}
