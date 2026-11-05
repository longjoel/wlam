#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_HOME="$PWD/toolchain/cargo" RUSTUP_HOME="$PWD/toolchain/rustup"
export PATH="$CARGO_HOME/bin:$PWD/toolchain/bin:$PATH"
./scripts/build-gdi-fixture.sh
cargo build --offline -p wlam-browser-host --target wasm32-unknown-unknown --target-dir target/hover-host-cache
cp target/hover-host-cache/wasm32-unknown-unknown/debug/wlam_browser_host.wasm target/wasm32-unknown-unknown/debug/wlam_browser_host.wasm
build_directory=$(mktemp -d "$PWD/target/gdi/build.XXXXXX")
cargo run --offline --target-dir target/hover-cache -p wlam-cli -- recompile target/gdi/gdi.exe -o "$build_directory/bundle"
mkdir -p target/gdi/bundle
cp "$build_directory/bundle/guest.wasm" "$build_directory/bundle/image.bin" "$build_directory/bundle/manifest.json" target/gdi/bundle/
node scripts/run-gdi-fixture.mjs
