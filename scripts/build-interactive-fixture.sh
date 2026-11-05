#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
output="$PWD/target/interactive"
mkdir -p "$output"
for dll in user32 gdi32 kernel32; do
  llvm-dlltool -m i386 -k -d "fixtures/interactive/$dll.def" -l "$output/$dll.lib"
done
fixture_opt="${WLAM_FIXTURE_OPT:--O0}"
case "$fixture_opt" in -O0|-O1|-O2) ;; *) echo "unsupported fixture optimization: $fixture_opt" >&2; exit 1;; esac
clang --target=i686-pc-windows-msvc "$fixture_opt" -fno-optimize-sibling-calls -fno-jump-tables -fno-builtin -fno-stack-protector -mno-sse -mno-sse2 -c fixtures/interactive/window.c -o "$output/window.obj"
toolchain/bin/ld.lld -flavor link /machine:x86 /subsystem:windows /entry:entry /nodefaultlib /safeseh:no /fixed /base:0x400000 "/out:$output/window.exe" "$output/window.obj" "$output/user32.lib" "$output/gdi32.lib" "$output/kernel32.lib"
echo "$output/window.exe"
