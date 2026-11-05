#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_HOME="$PWD/toolchain/cargo" RUSTUP_HOME="$PWD/toolchain/rustup"
export PATH="$CARGO_HOME/bin:$PWD/toolchain/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/hover-cache}"
hover_directory="${WLAM_HOVER_DIR:-/home/longjoel/Downloads/Windows 95 English/funstuff/hover}"
if [[ ! -f "$hover_directory/hover.exe" ]]; then
  echo "Set WLAM_HOVER_DIR to the original Hover directory." >&2
  exit 1
fi
hover_host_target="${WLAM_HOST_TARGET_DIR:-$PWD/target/hover-host-cache}"
cargo build --offline -p wlam-browser-host --target wasm32-unknown-unknown --target-dir "$hover_host_target"
mkdir -p target/wasm32-unknown-unknown/debug
hover_host_artifact="$hover_host_target/wasm32-unknown-unknown/debug/wlam_browser_host.wasm"
if ! [[ "$hover_host_artifact" -ef target/wasm32-unknown-unknown/debug/wlam_browser_host.wasm ]]; then
  cp "$hover_host_artifact" target/wasm32-unknown-unknown/debug/wlam_browser_host.wasm
fi
hover_build=$(mktemp -d "$PWD/target/hover-build.XXXXXX")
hover_seed_args=()
if [[ -n "${WLAM_HOVER_SEEDS:-}" ]]; then
  IFS=',' read -r -a hover_seed_values <<< "$WLAM_HOVER_SEEDS"
  for hover_seed in "${hover_seed_values[@]}"; do
    hover_seed_args+=(--seed "$hover_seed")
  done
fi
cargo run --offline -p wlam-cli -- recompile "$hover_directory/hover.exe" -o "$hover_build/bundle" "${hover_seed_args[@]}"
mkdir -p target/hover/bundle
cp "$hover_build/bundle/guest.wasm" "$hover_build/bundle/image.bin" "$hover_build/bundle/manifest.json" target/hover/bundle/
python3 scripts/pack-hover-assets.py "$hover_directory" target/hover/bundle
echo 'Ready: http://127.0.0.1:8765/apps/web/?manifest=/target/hover/bundle/manifest.json'
echo 'Run python3 scripts/serve-web.py --port 8765 if the server is not running.'
