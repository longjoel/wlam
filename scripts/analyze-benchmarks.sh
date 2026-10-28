#!/bin/sh
# Run `wlam analyze` over every benchmark executable and write a matrix.
#
# The matrix is the benchmark's contribution to the project: it is the exact,
# per-symbol definition of "done" for the host layer.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
export CARGO_HOME="$ROOT/toolchain/cargo"
export RUSTUP_HOME="$ROOT/toolchain/rustup"
export PATH="$ROOT/toolchain/cargo/bin:$PATH"
cd "$ROOT"
cargo build -q -p wlam-cli

status=0
for exe in "$ROOT"/benchmarks/*/bin/*.exe; do
    [ -f "$exe" ] || continue
    name=$(basename "$exe" .exe)
    out="$ROOT/benchmarks/$(basename "$(dirname "$(dirname "$exe")")")/compat-matrix-$name.md"
    echo "== $name"
    # Exit code 2 means blocking imports remain, which is expected and not an
    # error for this script: the matrix records them.
    "$ROOT/target/debug/wlam" analyze "$exe" --matrix "$out" >/dev/null || true
    grep -E '^\| [a-zA-Z]+ \| [0-9]+ \|' "$out" | sed 's/^/   /'
done
exit $status
