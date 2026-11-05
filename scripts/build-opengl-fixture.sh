#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

export CARGO_HOME="$PWD/toolchain/cargo" RUSTUP_HOME="$PWD/toolchain/rustup"
export PATH="$CARGO_HOME/bin:$PWD/toolchain/bin:$PATH"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"

output="$PWD/target/opengl-fixture"
mkdir -p "$output"
for dll in user32 gdi32 kernel32 opengl32; do
  llvm-dlltool -m i386 -k -d "fixtures/opengl/$dll.def" -l "$output/$dll.lib"
done

clang --target=i686-pc-windows-msvc -O0 -fno-optimize-sibling-calls \
  -fno-jump-tables -fno-builtin -fno-stack-protector -mno-sse -mno-sse2 \
  -c fixtures/opengl/window.c -o "$output/window.obj"
toolchain/bin/ld.lld -flavor link /machine:x86 /subsystem:windows /entry:entry \
  /nodefaultlib /safeseh:no /fixed /base:0x400000 \
  "/out:$output/window.exe" "$output/window.obj" \
  "$output/user32.lib" "$output/gdi32.lib" "$output/kernel32.lib" \
  "$output/opengl32.lib"

guest_cache="${CARGO_TARGET_DIR:-$PWD/target/tolympic-cache}"
host_cache="${WLAM_HOST_TARGET_DIR:-$PWD/target/tolympic-host-cache}"
build_directory=$(mktemp -d "$PWD/target/opengl-fixture-build.XXXXXX")
cargo run --offline --target-dir "$guest_cache" -p wlam-cli -- recompile \
  "$output/window.exe" -o "$build_directory/bundle"
cargo build --offline -p wlam-browser-host --target wasm32-unknown-unknown \
  --target-dir "$host_cache"
cargo build --offline -p wlam-browser-host --target wasm32-unknown-unknown \
  --release --target-dir "$host_cache"
host_artifact="$host_cache/wasm32-unknown-unknown/debug/wlam_browser_host.wasm"
host_output="$PWD/target/wasm32-unknown-unknown/debug/wlam_browser_host.wasm"
mkdir -p "$(dirname "$host_output")"
cp "$host_artifact" "$host_output"
host_release_artifact="$host_cache/wasm32-unknown-unknown/release/wlam_browser_host.wasm"
host_release_output="$PWD/target/wasm32-unknown-unknown/release/wlam_browser_host.wasm"
mkdir -p "$(dirname "$host_release_output")"
cp "$host_release_artifact" "$host_release_output"
mkdir -p "$output/bundle"
cp "$build_directory/bundle/guest.wasm" "$build_directory/bundle/image.bin" \
  "$build_directory/bundle/manifest.json" "$output/bundle/"
printf '%s\n' "$output/bundle/manifest.json"
