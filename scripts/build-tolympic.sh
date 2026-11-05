#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

export CARGO_HOME="$PWD/toolchain/cargo" RUSTUP_HOME="$PWD/toolchain/rustup"
export PATH="$CARGO_HOME/bin:$PWD/toolchain/bin:$PATH"

glint_directory="${WLAM_GLINT_DIR:-/home/longjoel/Downloads/glint_demos}"
tolympic_exe="$glint_directory/TOLYMPIC.exe"
if [[ ! -f "$tolympic_exe" ]]; then
  echo "Set WLAM_GLINT_DIR to the directory containing TOLYMPIC.exe." >&2
  exit 1
fi

guest_cache="${CARGO_TARGET_DIR:-$PWD/target/tolympic-cache}"
host_cache="${WLAM_HOST_TARGET_DIR:-$PWD/target/tolympic-host-cache}"
cargo build --offline -p wlam-browser-host --target wasm32-unknown-unknown --release --target-dir "$host_cache"

host_artifact="$host_cache/wasm32-unknown-unknown/release/wlam_browser_host.wasm"
host_output="$PWD/target/wasm32-unknown-unknown/release/wlam_browser_host.wasm"
mkdir -p "$(dirname "$host_output")"
if ! [[ "$host_artifact" -ef "$host_output" ]]; then
  cp "$host_artifact" "$host_output"
fi

build_directory=$(mktemp -d "$PWD/target/tolympic-build.XXXXXX")
cargo run --offline --target-dir "$guest_cache" -p wlam-cli -- recompile "$tolympic_exe" -o "$build_directory/bundle"
mkdir -p target/tolympic/bundle
cp "$build_directory/bundle/guest.wasm" "$build_directory/bundle/image.bin" "$build_directory/bundle/manifest.json" target/tolympic/bundle/

echo 'Ready: http://127.0.0.1:8765/apps/web/?renderer=webgl&manifest=/target/tolympic/bundle/manifest.json'
echo 'Run python3 scripts/serve-web.py --port 8765 if the server is not running.'
