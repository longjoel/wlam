// Manifest version 1 from wlam-sleigh::Compilation::bundle_image.
// No guessed Win32 behavior: missing host functions refuse at their call site.
export function importsFor(module, implementations = {}) {
  const imports = Object.create(null);
  for (const item of WebAssembly.Module.imports(module)) {
    if (item.kind !== 'function') throw Error(`Unsupported import kind: ${item.module}!${item.name} (${item.kind})`);
    imports[item.module] ??= Object.create(null);
    const implementation = Object.hasOwn(implementations, item.module) && Object.hasOwn(implementations[item.module], item.name) ? implementations[item.module][item.name] : null;
    imports[item.module][item.name] = implementation || ((args, returnAddress) => {
      throw Error(`Unimplemented browser host API: ${item.module}!${item.name} (args=0x${(args >>> 0).toString(16)}, return=0x${(returnAddress >>> 0).toString(16)})`);
    });
  }
  return imports;
}

export function loadImage(memory, manifest, image) {
  if (manifest.version !== 1) throw Error(`Unsupported bundle manifest version: ${manifest.version}`);
  const bytes = new Uint8Array(memory.buffer), view = new DataView(memory.buffer);
  const valid = (n, limit) => Number.isSafeInteger(n) && n >= 0 && n <= limit;
  const range = (start, size, limit) => valid(start, limit) && valid(size, limit - start);
  for (const segment of manifest.segments) {
    if (!range(segment.address, segment.mappedSize, bytes.length) || !range(segment.offset, segment.length, image.length) || segment.length > segment.mappedSize) throw Error('Invalid image segment range');
    bytes.fill(0, segment.address, segment.address + segment.mappedSize);
    bytes.set(image.subarray(segment.offset, segment.offset + segment.length), segment.address);
  }
  for (const reg of manifest.registers) {
    if (!range(reg.address, 8, bytes.length)) throw Error(`Register ${reg.name} is outside guest memory`);
    const value = reg.name === 'ESP' ? manifest.stack : reg.name === 'FS_OFFSET' ? manifest.tib : 0;
    if (!valid(value, 0xffffffff)) throw Error(`Invalid seed for ${reg.name}`);
    view.setUint32(reg.address, value, true); view.setUint32(reg.address + 4, 0, true);
  }
  if (!range(manifest.stack, 4, bytes.length) || !range(manifest.tib, 4, bytes.length)) throw Error('Invalid guest stack/TIB');
  view.setUint32(manifest.stack, 0, true);
  view.setUint32(manifest.tib, 0xffffffff, true);
}
