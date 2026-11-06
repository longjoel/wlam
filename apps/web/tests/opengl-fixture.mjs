import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { runBundle } from '../runner.mjs';
import { BUFFER_BYTES } from '../queue.mjs';

const root = process.cwd();
const bundle = `${root}/target/opengl-fixture/bundle`;
const manifest = JSON.parse(await readFile(`${bundle}/manifest.json`, 'utf8'));
const asArrayBuffer = bytes => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
const [moduleBytes, imageBytes, hostBytes] = await Promise.all([
  readFile(`${bundle}/${manifest.module}`),
  readFile(`${bundle}/${manifest.image}`),
  readFile(`${root}/target/wasm32-unknown-unknown/debug/wlam_browser_host.wasm`),
]);
const input = new SharedArrayBuffer(BUFFER_BYTES);
const frames = [];
const statuses = [];
const calls = [];
const gpuFrames = [];

runBundle({
  manifest,
  moduleBytes: asArrayBuffer(moduleBytes),
  imageBytes: asArrayBuffer(imageBytes),
  hostBytes: asArrayBuffer(hostBytes),
  input,
  onStatus: status => statuses.push(status),
  onApi: call => calls.push(call),
  onGpuFrame: frame => gpuFrames.push(frame),
  onFrame: frame => frames.push(frame),
});

assert.deepEqual(statuses.at(-1), { state: 'exited', code: 0 });
assert.equal(frames.length, 1);
const [{ width, height, pixels }] = frames;
assert.deepEqual([width, height], [32, 24]);
const pixel = (x, y) => Array.from(pixels.slice((y * width + x) * 4, (y * width + x) * 4 + 4));
assert.deepEqual(pixel(16, 12), [0, 255, 0, 255]);
assert.deepEqual(pixel(16, 8), [0, 0, 255, 255]);
assert.deepEqual(pixel(0, 0), [0, 0, 0, 255]);
for (const name of [
  'ChoosePixelFormat', 'SetPixelFormat', 'wglCreateContext', 'wglMakeCurrent',
  'glGetError', 'SwapBuffers', 'wglDeleteContext', 'ReleaseDC', 'DestroyWindow',
]) assert(calls.some(call => call.name === name), `${name} was not reached`);
assert.equal(calls.filter(call => call.name === 'SwapBuffers').length, 1);
assert.equal(gpuFrames.length, 1);
const packet = gpuFrames[0].words;
assert.deepEqual([...packet.slice(0, 5)], [0x574c474c, 1, 32, 24, 3]);
assert.equal(packet[5], 0); // GL clear
assert.equal(packet[12], 1); // ordered triangle draw
assert.deepEqual([...packet.slice(13, 15)], [3, 0x0004]); // triangle vertex count and mode
assert.deepEqual([...packet.slice(71, 74)], [1, 1, 0x0000]); // point draw, one vertex, GL_POINTS
console.log('OpenGL PE fixture passed: WGL lifecycle, glGetError ABI, CPU triangle/point pixels, ordered GPU packets, one swap and clean exit.');
