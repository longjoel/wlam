#!/bin/sh
# Fetch the third-party reference material this project is built against.
#
# None of it is vendored in git: Ghidra is Apache-2.0 and rsleigh is
# Apache-2.0, but the Ghidra distribution is ~450 MB and rsleigh needs a code
# generation pass, so both are fetched and built here instead.
#
# Usage: scripts/fetch-deps.sh [ghidra|rsleigh|all]
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
TOOLCHAIN="$ROOT/toolchain"
RSLEIGH_REV="7d80fe56b459a06eceea750220a51832a3697942"
GHIDRA_VERSION="11.4.2"
GHIDRA_ZIP="ghidra_${GHIDRA_VERSION}_PUBLIC_20250826.zip"

fetch_ghidra() {
    # Only the x86 processor module is needed: the compiled Sleigh spec
    # (x86.sla) and its sources, for reference when auditing our lifter.
    mkdir -p "$TOOLCHAIN/ghidra"
    cd "$TOOLCHAIN/ghidra"
    if [ ! -f "$GHIDRA_ZIP" ]; then
        curl -sSL -o "$GHIDRA_ZIP" \
            "https://github.com/NationalSecurityAgency/ghidra/releases/download/Ghidra_${GHIDRA_VERSION}_build/$GHIDRA_ZIP"
    fi
    unzip -o -q "$GHIDRA_ZIP" "ghidra_${GHIDRA_VERSION}_PUBLIC/Ghidra/Processors/x86/*" -d extracted
    echo "Ghidra x86 module: $TOOLCHAIN/ghidra/extracted/ghidra_${GHIDRA_VERSION}_PUBLIC/Ghidra/Processors/x86/data/languages"
}

fetch_rsleigh() {
    # rsleigh ships generated decoders as empty shells; they must be
    # generated from the Sleigh sources first (~1 min per architecture).
    cd "$TOOLCHAIN"
    if [ ! -d rsleigh ]; then
        git clone --no-checkout --depth 1 https://github.com/ShaneBreazeale/rsleigh
        git -C rsleigh fetch --depth 1 origin "$RSLEIGH_REV"
        git -C rsleigh checkout --detach "$RSLEIGH_REV"
    fi
    cd rsleigh
    # Local fixes to rsleigh live here as patches, because this directory is a
    # throwaway clone and everything in it is gitignored. Without this step a
    # fresh checkout silently loses them.
    #
    # 0001: SUBPIECE's `lsb` is a byte count, but the codegen divided the
    #       `trunk` value by 8 as if it were bits, so `tmp(4)` became `lsb = 0`
    #       and both halves of a dword MUL collapsed onto the low half
    #       (EDX = EAX). See docs/sleigh-notes.md.
    # 0002: apply FS/GS bases to absolute displacement addressing as well.
    for patch in "$ROOT/scripts/patches"/*.patch; do
        [ -e "$patch" ] || continue
        if git apply --reverse --check "$patch" 2>/dev/null; then
            echo "rsleigh: already applied $(basename "$patch")"
        else
            echo "rsleigh: applying $(basename "$patch")"
            git apply "$patch" || {
                echo "rsleigh: FAILED to apply $(basename "$patch")" >&2
                echo "  the upstream commit probably moved; re-derive the patch" >&2
                exit 1
            }
        fi
    done
    export CARGO_HOME="$TOOLCHAIN/cargo"
    export RUSTUP_HOME="$TOOLCHAIN/rustup"
    export PATH="$TOOLCHAIN/cargo/bin:$PATH"
    cargo run --release -p rsleigh-generate
    cargo build --release -p rsleigh-cli
    echo "rsleigh CLI: $TOOLCHAIN/rsleigh/target/release/rsleigh"
}

case "${1:-all}" in
    ghidra) fetch_ghidra ;;
    rsleigh) fetch_rsleigh ;;
    all) fetch_ghidra; fetch_rsleigh ;;
    *) echo "usage: $0 [ghidra|rsleigh|all]" >&2; exit 1 ;;
esac
