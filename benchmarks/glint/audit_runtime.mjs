// Bounded interaction trace for the external TOLYMPIC.exe target.
// Run from the repository root after scripts/build-tolympic.sh.
import { readFile, writeFile } from 'node:fs/promises';
import { writeFileSync } from 'node:fs';
import { runBundle } from '../../apps/web/runner.mjs';
import { enqueue, BUFFER_BYTES } from '../../apps/web/queue.mjs';

const base = process.cwd();
const bundle = `${base}/target/tolympic/bundle`;
const manifest = JSON.parse(await readFile(`${bundle}/manifest.json`, 'utf8'));
const [moduleBytes, imageBytes, hostBytes] = await Promise.all([
  readFile(`${bundle}/${manifest.module}`),
  readFile(`${bundle}/${manifest.image}`),
  readFile(`${base}/target/wasm32-unknown-unknown/debug/wlam_browser_host.wasm`),
]);
const asArrayBuffer = bytes => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
const input = new SharedArrayBuffer(BUFFER_BYTES);
const apiCalls = [];
const frames = [];
let frameCount = 0;
let resizeSent = false;

function pointerValues(memory, address, count, type = 'f32') {
  const start = address >>> 0;
  const bytes = type === 'u8' ? count : count * 4;
  if (start + bytes > memory.buffer.byteLength) return null;
  const view = new DataView(memory.buffer);
  return Array.from({ length: count }, (_, i) => type === 'u8'
    ? view.getUint8(start + i)
    : view.getFloat32(start + i * 4, true));
}

function pixelFormat(memory, address) {
  const start = address >>> 0;
  if (start + 40 > memory.buffer.byteLength) return null;
  const view = new DataView(memory.buffer);
  const flags = view.getUint32(start + 4, true);
  return {
    size: view.getUint16(start, true), version: view.getUint16(start + 2, true), flags,
    drawToWindow: (flags & 0x04) !== 0, supportOpenGl: (flags & 0x20) !== 0,
    doubleBuffer: (flags & 0x01) !== 0, pixelType: view.getUint8(start + 8),
    colorBits: view.getUint8(start + 9), alphaBits: view.getUint8(start + 16),
    depthBits: view.getUint8(start + 23), stencilBits: view.getUint8(start + 24),
  };
}

function message(memory, address) {
  const start = address >>> 0;
  if (start + 28 > memory.buffer.byteLength) return null;
  const view = new DataView(memory.buffer);
  return {
    window: view.getUint32(start, true), message: view.getUint32(start + 4, true),
    wparam: view.getUint32(start + 8, true), lparam: view.getUint32(start + 12, true),
    time: view.getUint32(start + 16, true), x: view.getInt32(start + 20, true),
    y: view.getInt32(start + 24, true),
  };
}

function enrichCall(call, memory) {
  const details = {};
  if (call.name === 'GetLocalTime') {
    const start = call.args[0] >>> 0;
    if (start + 16 <= memory.buffer.byteLength) {
      const view = new DataView(memory.buffer);
      details.systemTime = Array.from({ length: 8 }, (_, i) => view.getUint16(start + i * 2, true));
    }
  }
  if (call.name === 'glColor3ubv') details.pointedValues = pointerValues(memory, call.args[0], 3, 'u8');
  if (call.name === 'glLightfv') details.pointedValues = pointerValues(memory, call.args[2], call.args[1] === 0x1204 ? 3 : 4);
  if (call.name === 'glLightModelfv') details.pointedValues = pointerValues(memory, call.args[1], call.args[0] === 0x0b53 ? 4 : 1);
  if (call.name === 'glMaterialfv') details.pointedValues = pointerValues(memory, call.args[2], call.args[1] === 0x1601 ? 1 : 4);
  if (call.name === 'glFogfv') details.pointedValues = pointerValues(memory, call.args[1], call.args[0] === 0x0b66 ? 4 : 1);
  if (call.name === 'ChoosePixelFormat') details.pixelFormat = pixelFormat(memory, call.args[1]);
  if (call.name === 'DescribePixelFormat') details.pixelFormat = pixelFormat(memory, call.args[3]);
  if (call.name === 'SetPixelFormat') details.pixelFormat = pixelFormat(memory, call.args[2]);
  if (['GetMessageA', 'PeekMessageA', 'TranslateMessage', 'DispatchMessageA'].includes(call.name)) {
    details.message = message(memory, call.args[0]);
  }
  if (call.name === 'GetClientRect') {
    const start = call.args[1] >>> 0;
    if (start + 16 <= memory.buffer.byteLength) {
      const view = new DataView(memory.buffer);
      details.clientRect = Array.from({ length: 4 }, (_, i) => view.getInt32(start + i * 4, true));
    }
  }
  const doubleCounts = { glClearDepth: 1, gluPerspective: 4, gluLookAt: 9, glOrtho: 6 };
  if (doubleCounts[call.name]) {
    const view = new DataView(new ArrayBuffer(8));
    details.doubleValues = Array.from({ length: doubleCounts[call.name] }, (_, i) => {
      view.setUint32(0, call.args[i * 2], true);
      view.setUint32(4, call.args[i * 2 + 1], true);
      return view.getFloat64(0, true);
    });
  }
  return Object.keys(details).length ? { ...call, ...details } : call;
}

runBundle({
  manifest,
  moduleBytes: asArrayBuffer(moduleBytes),
  imageBytes: asArrayBuffer(imageBytes),
  hostBytes: asArrayBuffer(hostBytes),
  input,
  onStatus: () => {},
  onApi: (call, memory) => apiCalls.push(enrichCall({ frame: frameCount, ...call }, memory)),
  onFrame: ({ window, width, height, pixels }) => {
    frameCount++;
    frames.push({ frame: frameCount, width, height });
    if (frameCount === 69) writeFileSync('/tmp/tolympic-before-d.rgba', Buffer.from(pixels));
    if (frameCount === 90) writeFileSync('/tmp/tolympic-resized.rgba', Buffer.from(pixels));
    if (frameCount === 103) writeFileSync('/tmp/tolympic-after-d-blend.rgba', Buffer.from(pixels));
    if (frameCount === 109) writeFileSync('/tmp/tolympic-after-d-opaque.rgba', Buffer.from(pixels));
    if (frameCount === 1) enqueue(input, { window, message: 0x0100, wparam: 0x20 }); // Space
    if (frameCount === 70) enqueue(input, { window, message: 0x0100, wparam: 0x42 }); // B
    if (frameCount === 80 && !resizeSent) {
      resizeSent = true;
      enqueue(input, { window, message: 0xffff_fffe, wparam: 600, lparam: 450 });
    }
    if (frameCount === 100) enqueue(input, { window, message: 0x0100, wparam: 0x44 }); // VK_D; TranslateMessage produces d
    if (frameCount === 105) enqueue(input, { window, message: 0x0100, wparam: 0x42 }); // B toggles blending off
    if (frameCount === 110) enqueue(input, { window, message: 0x0100, wparam: 0x1b }); // Escape
  },
});

const counts = {};
for (const call of apiCalls) {
  const key = `${call.library}!${call.name}`;
  counts[key] = (counts[key] ?? 0) + 1;
}
const trace = {
  target: 'external TOLYMPIC.exe',
  framesPresented: frameCount,
  controlsSent: ['Space at frame 1', 'B at frame 70', 'resize to 600x450 at frame 80', 'VK_D at frame 100', 'B at frame 105', 'Escape at frame 110'],
  apiCallCount: apiCalls.length,
  distinctApis: Object.keys(counts).length,
  counts,
  frames,
  apiCalls,
};
await writeFile(`${base}/benchmarks/glint/tolympic-api-trace.json`, `${JSON.stringify(trace, null, 2)}\n`);
console.log(JSON.stringify({
  framesPresented: frameCount,
  apiCallCount: apiCalls.length,
  distinctApis: Object.keys(counts).length,
  glGetError: counts['OPENGL32.dll!glGetError'] ?? 0,
  glIsEnabled: counts['OPENGL32.dll!glIsEnabled'] ?? 0,
  swapBuffers: counts['GDI32.dll!SwapBuffers'] ?? 0,
  finalSize: frames.at(-1),
}));
