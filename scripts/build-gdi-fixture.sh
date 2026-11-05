#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
output="$PWD/target/gdi"
mkdir -p "$output"
for dll in gdi32 kernel32; do
  llvm-dlltool -m i386 -k -d "fixtures/gdi/$dll.def" -l "$output/$dll.lib"
done
clang --target=i686-pc-windows-msvc -O0 -fno-optimize-sibling-calls -fno-jump-tables -fno-builtin -fno-stack-protector -mno-sse -mno-sse2 -c fixtures/gdi/gdi.c -o "$output/gdi.obj"
toolchain/bin/ld.lld -flavor link /machine:x86 /subsystem:console /entry:entry /nodefaultlib /safeseh:no /fixed /base:0x400000 "/out:$output/gdi.exe" "$output/gdi.obj" "$output/gdi32.lib" "$output/kernel32.lib"
echo "$output/gdi.exe"
