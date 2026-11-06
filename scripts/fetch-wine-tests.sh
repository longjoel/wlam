#!/usr/bin/env bash
# Fetch Wine's gdi32 conformance tests without vendoring the Wine tree.
#
# The checkout is pinned rather than tracking master: a test result must name
# the exact source revision it was measured against. Override WINE_COMMIT to
# compare another revision.
set -euo pipefail
cd "$(dirname "$0")/.."

commit="${WINE_COMMIT:-c42c1532a8b8a767d5e23b527ea874368c202ed0}"
destination="${WINE_TESTS_DIR:-$PWD/target/wine-tests/wine}"

if [[ -f "$destination/dlls/gdi32/tests/brush.c" ]]; then
  echo "Wine tests already present: $destination"
  exit 0
fi
if [[ -e "$destination" ]]; then
  echo "$destination exists but is not a Wine checkout" >&2
  exit 1
fi

mkdir -p "$(dirname "$destination")"
git init -q "$destination"
git -C "$destination" remote add origin https://github.com/wine-mirror/wine
git -C "$destination" fetch --depth 1 origin "$commit"
git -C "$destination" checkout -q --detach FETCH_HEAD
git -C "$destination" sparse-checkout init --cone
git -C "$destination" sparse-checkout set dlls/gdi32/tests include include/ddk
echo "Fetched Wine commit $commit to $destination"
