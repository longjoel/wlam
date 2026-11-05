#!/usr/bin/env bash
# Default: all active workspace tests, then validation of both real images.
# --fast: validation only; this does not establish correct guest execution.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_HOME="$PWD/toolchain/cargo" RUSTUP_HOME="$PWD/toolchain/rustup"
export PATH="$CARGO_HOME/bin:$PATH"
case "${1:-}" in
  '') cargo test --workspace --offline ;;
  --fast) echo 'Validation only (execution tests omitted)' ;;
  *) echo 'usage: scripts/loop.sh [--fast]' >&2; exit 1 ;;
esac
fail=0
for image in north-star/bin/Uxn32.exe benchmarks/minesweeper/bin/minesweeper-x86.exe; do
  echo "IMAGE: $image"
  if ! out=$(cargo run --offline -q -p wlam-sleigh --example progress -- "$PWD/$image"); then
    echo "$out"
    fail=1
    continue
  fi
  echo "$out"
  if ! [[ "$out" == *'RESULT: VALIDATED '* ]] || [[ "$out" == *'WARN:'* ]]; then
    fail=1
  fi
done
exit "$fail"
