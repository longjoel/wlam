#!/usr/bin/env python3
"""Package the native CLI output with the Studio browser diagnostic runtime."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--input', type=Path, required=True)
    parser.add_argument('--assets', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    binary = os.environ.get('WLAM_BIN', str(root / 'target/debug/wlam'))
    # CLI owns creation of its output directory. Studio owns the outer directory.
    with tempfile.TemporaryDirectory(dir=args.output.parent) as temporary:
        compiled = Path(temporary) / 'compiled'
        subprocess.run([binary, 'recompile', str(args.input), '-o', str(compiled)], check=True)
        for name in ['guest.wasm', 'image.bin', 'manifest.json']:
            shutil.copyfile(compiled / name, args.output / name)
    shutil.copytree(Path(__file__).with_name('runtime'), args.output, dirs_exist_ok=True)
    shutil.copytree(args.assets, args.output / 'assets')
    print('Browser bundle created. The included diagnostic runtime refuses unimplemented Win32 APIs.')


if __name__ == '__main__':
    main()
