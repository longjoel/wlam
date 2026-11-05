#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_HOME="$PWD/toolchain/cargo" RUSTUP_HOME="$PWD/toolchain/rustup"
export PATH="$CARGO_HOME/bin:$PATH"
./scripts/build-interactive-fixture.sh
cargo build --offline -p wlam-browser-host --target wasm32-unknown-unknown
build_dir=$(mktemp -d "$PWD/target/web-build.XXXXXX")
cargo run -q --offline -p wlam-cli -- recompile target/interactive/window.exe -o "$build_dir/bundle"
mkdir -p target/interactive/bundle
cp "$build_dir/bundle/guest.wasm" target/interactive/bundle/guest.wasm
cp "$build_dir/bundle/image.bin" target/interactive/bundle/image.bin
cp "$build_dir/bundle/manifest.json" target/interactive/bundle/manifest.json
echo 'Ready: python3 scripts/serve-web.py'
