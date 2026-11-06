#!/usr/bin/env bash
# Compile and run one Wine gdi32 conformance test through the recompiler.
#
#   ./scripts/wine-tests.sh [brush|dc|pen|clipping|palette|mapping|...]
#
# The test source is Wine's, unchanged except for the tiny reporting header in
# scripts/wine-tests/shim/wine/test.h. All imports are linked through one
# synthetic WLAMTEST.dll; the runner resolves them by symbol name.
set -euo pipefail
cd "$(dirname "$0")/.."

test_name="${1:-brush}"
wine="${WINE_TESTS_DIR:-$PWD/target/wine-tests/wine}"
source_file="$wine/dlls/gdi32/tests/$test_name.c"
if [[ ! -f "$source_file" ]]; then
  if [[ ! -d "$wine" ]]; then
    ./scripts/fetch-wine-tests.sh
  fi
  if [[ ! -f "$source_file" ]]; then
    echo "No such Wine gdi32 test: $test_name" >&2
    echo "Available: $(cd "$wine/dlls/gdi32/tests" && ls *.c | sed 's/\.c$//' | tr '\n' ' ')" >&2
    exit 2
  fi
fi

if [[ -d "$wine/.git" && ! -f "$wine/include/ddk/d3dkmthk.h" ]]; then
  git -C "$wine" sparse-checkout add include/ddk
fi

export CARGO_HOME="$PWD/toolchain/cargo" RUSTUP_HOME="$PWD/toolchain/rustup"
export PATH="$PWD/toolchain/cargo/bin:$PWD/toolchain/bin:$PATH"
build="$PWD/target/wine-tests/build/$test_name"
rm -rf "$build"
mkdir -p "$build"
shim="$PWD/scripts/wine-tests/shim"

compile() {
  clang --target=i686-pc-windows-msvc -O0 -fno-stack-protector \
    -fno-optimize-sibling-calls -fno-jump-tables -fno-builtin \
    -mno-stack-arg-probe -mno-sse -mno-sse2 \
    -Xclang -target-feature -Xclang -cmov "$@"
}

compile -I "$shim" -I "$shim/include" -I "$wine/include" \
  -c "$source_file" -o "$build/$test_name.o"
compile -DTEST_FUNC="$test_name" -DTEST_NAME="\"$test_name\"" \
  -I "$shim" -I "$shim/include" -c scripts/wine-tests/runner.c -o "$build/runner.o"

{
  llvm-nm -u "$build/$test_name.o" || true
  llvm-nm -u "$build/runner.o" || true
} > "$build/undefined.txt"

python3 - "$build/undefined.txt" "$build/wlamtest.def" <<'PY'
import re
import sys
from pathlib import Path

symbols = set()
for line in Path(sys.argv[1]).read_text().splitlines():
    token = line.split()[-1] if line.split() else ''
    if token.startswith('__imp_'):
        name = token[len('__imp_'):]
        if name.startswith('_'):
            name = name[1:]
        symbols.add(name)
    elif token.startswith('_') and not token.startswith('__'):
        name = token[1:]
        if name.startswith(('func_', 'wlam_test_')):
            continue
        # cdecl import (memcpy, memset, ...)
        symbols.add(name)
symbols.discard('_wlam_test_ok')
symbols = sorted(symbols, key=lambda s: (s.lower(), s))
Path(sys.argv[2]).write_text('LIBRARY WLAMTEST.dll\nEXPORTS\n' + '\n'.join(symbols) + '\n')
print(f'{len(symbols)} imports')
PY

llvm-dlltool -m i386 -k -d "$build/wlamtest.def" -l "$build/wlamtest.lib"
ld.lld -flavor link /machine:x86 /subsystem:console /entry:entry /nodefaultlib \
  /fixed /base:0x400000 /safeseh:no \
  "/out:$build/$test_name.exe" "$build/$test_name.o" "$build/runner.o" "$build/wlamtest.lib"

cargo build --offline -q -p wlam-cli
cargo build --offline -q -p wlam-browser-host --target wasm32-unknown-unknown
./target/debug/wlam recompile "$build/$test_name.exe" -o "$build/bundle" > "$build/recompile.log" 2>&1

node scripts/wine-tests/run.mjs "$build/bundle"
