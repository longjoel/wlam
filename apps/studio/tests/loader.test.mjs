import assert from 'node:assert/strict';
import {test} from 'node:test';
import {importsFor, loadImage} from '../runtime/loader.mjs';

test('loader maps segments, zeros BSS, and seeds register snapshot and TIB', () => {
  const memory = new WebAssembly.Memory({initial:1});
  new Uint8Array(memory.buffer).fill(0xaa);
  const manifest = {version:1, stack:0x700, tib:0x600, segments:[{address:0x1000, offset:1, length:2, mappedSize:4}], registers:[{name:'ESP',address:0x100},{name:'FS_OFFSET',address:0x108},{name:'EAX',address:0x110}]};
  loadImage(memory, manifest, new Uint8Array([99, 42, 43]));
  const view = new DataView(memory.buffer);
  assert.deepEqual([...new Uint8Array(memory.buffer, 0x1000, 4)], [42,43,0,0]);
  assert.equal(view.getUint32(0x100, true), 0x700);
  assert.equal(view.getUint32(0x104, true), 0);
  assert.equal(view.getUint32(0x108, true), 0x600);
  assert.equal(view.getUint32(0x110, true), 0);
  assert.equal(view.getUint32(0x600, true), 0xffffffff);
  assert.equal(view.getUint32(0x700, true), 0);
});

test('loader rejects invalid versions and segment ranges', () => {
  const memory = new WebAssembly.Memory({initial:1});
  const manifest = {version:1, stack:256, tib:512, registers:[], segments:[]};
  assert.throws(() => loadImage(memory, {...manifest,version:2}, new Uint8Array()), /version/);
  for (const segment of [{address:65535,offset:0,length:2,mappedSize:2},{address:1,offset:1,length:2,mappedSize:2},{address:1,offset:0,length:2,mappedSize:1},{address:-1,offset:0,length:1,mappedSize:1}]) {
    assert.throws(() => loadImage(memory, {...manifest,segments:[segment]}, new Uint8Array([1,2])), /range/);
  }
});

test('real wasm imports get named refusals or explicitly supplied implementations', () => {
  const string = s => [s.length, ...new TextEncoder().encode(s)];
  const section = (id, data) => [id, data.length, ...data];
  const bytes = new Uint8Array([0,97,115,109,1,0,0,0,
    ...section(1,[1,0x60,2,0x7f,0x7f,1,0x7f]),
    ...section(2,[1,...string('KERNEL32.dll'),...string('MissingAPI'),0,0])]);
  const module = new WebAssembly.Module(bytes);
  const imports = importsFor(module);
  assert.throws(() => imports['KERNEL32.dll'].MissingAPI(32,64), /KERNEL32.dll!MissingAPI.*args=0x20/);
  assert.doesNotThrow(() => new WebAssembly.Instance(module, imports));
  const provided = importsFor(module, {'KERNEL32.dll':{MissingAPI:() => 42}});
  assert.equal(provided['KERNEL32.dll'].MissingAPI(), 42);
});
