"""Explicit native CLI integration: a tiny real PE stores 42, then returns.

Run from the repository root: python3 apps/studio/tests/native_smoke.py
Requires target/debug/wlam (or WLAM_BIN) and Node. Does not require a browser.
"""
import io
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import time
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from server import ROOT, Studio


def pe_fixture():
    image = bytearray(1024)
    def put(offset, fmt, *values):
        struct.pack_into('<' + fmt, image, offset, *values)
    image[:2] = b'MZ'
    put(0x3c, 'I', 0x80)
    image[0x80:0x84] = b'PE\x00\x00'
    put(0x84, 'HHIIIHH', 0x14c, 1, 0, 0, 0, 224, 0x102)
    opt = 0x98
    put(opt, 'H', 0x10b)
    for offset, value in [(16,0x1000),(20,0x1000),(28,0x400000),(32,0x1000),(36,0x200),(56,0x3000),(60,0x200),(72,0x100000),(76,0x1000),(80,0x100000),(84,0x1000),(92,16)]:
        put(opt + offset, 'I', value)
    put(opt+68, 'H', 3)
    section = opt + 224
    image[section:section+8] = b'.text\x00\x00\x00'
    put(section+8, 'IIII', 0x2000, 0x1000, 0x200, 0x200)
    put(section+36, 'I', 0xe0000020)
    # mov eax,42; mov [0x402000],eax; ret
    image[0x200:0x20b] = bytes.fromhex('b82a000000a300204000c3')
    return bytes(image)


with tempfile.TemporaryDirectory() as temporary:
    studio_path = Path(__file__).resolve().parents[1]
    binary = os.environ.get('WLAM_BIN', str(ROOT / 'target/debug/wlam'))
    compiler = [sys.executable, str(studio_path / 'bundle.py'), '--input', '{input}', '--assets', '{assets}', '--output', '{output}']
    studio = Studio(Path(temporary), binary, compiler)
    try:
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, 'w') as archive:
            archive.writestr('sample.exe', pe_fixture())
            archive.writestr('level.txt', 'asset retained')
        project = studio.upload(buffer.getvalue(), 'Native smoke.zip')
        studio.enqueue(project['id'], project['selected'], 'build')
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            project = studio.get(project['id'])
            if project['state'] not in ('queued', 'analyzing', 'compiling'):
                break
            time.sleep(.05)
        assert project['state'] == 'ready', project
        bundle = studio.folder(project['id']) / 'output'
        assert (bundle / 'assets/level.txt').read_text() == 'asset retained'
        subprocess.run(['node', '--input-type=module', '-e', '''
import fs from 'node:fs';
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const directory = process.argv[1];
const {importsFor, loadImage} = await import(pathToFileURL(directory + '/loader.mjs'));
const manifest = JSON.parse(fs.readFileSync(directory + '/manifest.json'));
const module = await WebAssembly.compile(fs.readFileSync(directory + '/guest.wasm'));
const instance = await WebAssembly.instantiate(module, importsFor(module));
loadImage(instance.exports.memory, manifest, new Uint8Array(fs.readFileSync(directory + '/image.bin')));
instance.exports.entry();
assert.equal(new DataView(instance.exports.memory.buffer).getUint32(0x402000, true), 42);
console.log('PASS: real PE -> native compiler -> browser bundle -> wasm entry -> guest memory = 42');
''', str(bundle)], check=True)
    finally:
        studio.pool.shutdown(wait=True)
