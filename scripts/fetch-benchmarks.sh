#!/bin/sh
# Fetch benchmark executables into benchmarks/*/bin/.
#
# They are not tracked in git: they are third-party build outputs, and storing
# binaries in a source repo makes review harder. Each script pins a release and
# verifies the sha256 recorded in that benchmark's README.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BASE="https://github.com/enis1enis2/minesweeper-classic/releases/download/v1.0.1"
DEST="$ROOT/benchmarks/minesweeper/bin"

mkdir -p "$DEST"
fetch() {
    url="$1"; out="$2"; want="$3"
    if [ ! -f "$DEST/$out" ]; then
        curl -sSL -m 300 -o "$DEST/$out" "$url"
    fi
    got=$(sha256sum "$DEST/$out" | cut -d' ' -f1)
    if [ "$got" != "$want" ]; then
        echo "checksum mismatch for $out: got $got want $want" >&2
        rm -f "$DEST/$out"
        exit 1
    fi
    echo "ok $out"
}

fetch "$BASE/minesweeper-x86.exe" minesweeper-x86.exe \
    02220c5683e78e291d3cf6ff3fcfae0ae0fdd4eeafd3fd2664c4c7530cbb9ab3
fetch "$BASE/minesweeper-x64.exe" minesweeper-x64.exe \
    4d43336d9a53d1ed1e6a116c2cb3a0e21521eb702724d4a3ac003957a9739d09
