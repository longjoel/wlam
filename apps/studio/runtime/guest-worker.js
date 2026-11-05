import {importsFor, loadImage} from './loader.mjs';
const send = (type, message) => postMessage({type, message});
async function checked(url) { const r = await fetch(url); if (!r.ok) throw Error(`Failed to load ${url}: HTTP ${r.status}`); return r; }
onmessage = async ({data}) => {
  try {
    const manifestURL = new URL(data.manifest, location.href);
    const manifest = await (await checked(manifestURL)).json();
    if (manifest.version !== 1) throw Error(`Unsupported manifest version ${manifest.version}`);
    const [wasmResponse, imageResponse] = await Promise.all([checked(new URL(manifest.module, manifestURL)), checked(new URL(manifest.image, manifestURL))]);
    const module = await WebAssembly.compile(await wasmResponse.arrayBuffer());
    const instance = await WebAssembly.instantiate(module, importsFor(module));
    if (!(instance.exports.memory instanceof WebAssembly.Memory) || typeof instance.exports.entry !== 'function') throw Error('Guest must export memory and entry');
    loadImage(instance.exports.memory, manifest, new Uint8Array(await imageResponse.arrayBuffer()));
    send('progress', `Mapped guest image. ${WebAssembly.Module.imports(module).length} host imports declared. Calling entry…`);
    instance.exports.entry();
    send('done', 'Guest entry returned. This does not verify application compatibility.');
  } catch (error) { send('error', error.message); }
};
