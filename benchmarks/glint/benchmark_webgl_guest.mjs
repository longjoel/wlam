// Measure guest and GL packet generation without browser rendering or rAF pacing.
// Run after scripts/build-tolympic.sh from the repository root.
import { readFile } from 'node:fs/promises';
import { performance } from 'node:perf_hooks';
import { runBundle } from '../../apps/web/runner.mjs';
import { enqueue, BUFFER_BYTES } from '../../apps/web/queue.mjs';

const root = process.cwd();
const bundle = `${root}/target/tolympic/bundle`;
const manifest = JSON.parse(await readFile(`${bundle}/manifest.json`, 'utf8'));
const hostPath = process.env.WLAM_HOST_WASM
  ?? `${root}/target/wasm32-unknown-unknown/release/wlam_browser_host.wasm`;
const [moduleBytes, imageBytes, hostBytes] = await Promise.all([
  readFile(`${bundle}/${manifest.module}`),
  readFile(`${bundle}/${manifest.image}`),
  readFile(hostPath),
]);
const asArrayBuffer = bytes => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
const input = new SharedArrayBuffer(BUFFER_BYTES);
let frames = 0;
let packetBytes = 0;
const started = performance.now();

runBundle({
  manifest, moduleBytes: asArrayBuffer(moduleBytes), imageBytes: asArrayBuffer(imageBytes),
  hostBytes: asArrayBuffer(hostBytes), input, gpuOnly: true,
  onStatus: () => {},
  onGpuFrame: ({ window, words }) => {
    frames++;
    packetBytes += words.byteLength;
    if (frames === 1) enqueue(input, { window, message: 0x0100, wparam: 0x20 });
    if (frames === 70) enqueue(input, { window, message: 0x0100, wparam: 0x42 });
    if (frames === 80) enqueue(input, { window, message: 0xffff_fffe, wparam: 600, lparam: 450 });
    if (frames === 100) enqueue(input, { window, message: 0x0100, wparam: 0x44 });
    if (frames === 105) enqueue(input, { window, message: 0x0100, wparam: 0x42 });
    if (frames === 110) enqueue(input, { window, message: 0x0100, wparam: 0x1b });
  },
});

const seconds = (performance.now() - started) / 1000;
console.log(JSON.stringify({ frames, seconds, framesPerSecond: frames / seconds, averagePacketBytes: packetBytes / frames }));
