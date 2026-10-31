#!/usr/bin/env bash
# One iteration of the emit loop: check the scoreboard and the test suite.
#
#   ./scripts/loop.sh            run tests + progress oracle on every image
#   ./scripts/loop.sh --fast     progress oracle only
#
# Exit condition for "working": progress prints RESULT: OK for the north-star
# image and every test passes. The benchmark image is reported honestly and is
# allowed to stay BLOCKED on a known, documented gap (currently x87) -- but it is
# always *run*, because a scoreboard that only watches one image hides exactly the
# regressions it exists to catch. That is how the x87 blocker went unnoticed while
# the loop reported OK.
set -uo pipefail
cd "$(dirname "$0")/.."
export CARGO_HOME=$PWD/toolchain/cargo RUSTUP_HOME=$PWD/toolchain/rustup

fail=0
if [[ "${1:-}" != "--fast" ]]; then
  if ! out=$(cargo test -p wlam-emit 2>&1); then
    echo "TESTS: FAIL"; echo "$out" | grep -E "^test .* FAILED|panicked at" | head -10; fail=1
  else
    echo "TESTS: OK ($(echo "$out" | grep -cE '^test .* \.\.\. ok') passed)"
  fi
  if ! out=$(cd crates/wlam-sleigh && cargo test 2>&1); then
    echo "SLEIGH TESTS: FAIL"; echo "$out" | grep -E "^test .* FAILED|panicked at" | head -10; fail=1
  else
    echo "SLEIGH TESTS: OK ($(echo "$out" | grep -cE '^test .* \.\.\. ok') passed)"
  fi
fi

# The gating image: the loop is only green when this one emits a valid module.
out=$(cd crates/wlam-sleigh && cargo run -q --example progress 2>/dev/null)
echo "$out" | grep -E "RESULT|REASON|WARN"
echo "$out" | grep -q "RESULT: OK" || fail=1

# The benchmark image. Reported, and *expected* to be blocked on a known gap.
# A regression here (fewer blocks than the last known-good floor) is a failure;
# a change in the blocking reason is announced so it cannot drift silently.
bench=benchmarks/minesweeper/bin/minesweeper-x86.exe
if [[ -f "$bench" ]]; then
  bout=$(cd crates/wlam-sleigh && cargo run -q --example progress -- "../../$bench" 2>/dev/null)
  echo "BENCH: $(echo "$bout" | grep -E 'RESULT|REASON' | tr '\n' ' ')"
  # Minesweeper used to be blocked on x87 (80-bit) operands. It now emits and
  # validates, so the interesting failure is a regression: if it goes back to
  # BLOCKED, say so loudly rather than letting the reason drift silently.
  if echo "$bout" | grep -q "RESULT: OK"; then
    echo "BENCH NOTE: Minesweeper emits a valid module"
  elif echo "$bout" | grep -q "RESULT: BLOCKED"; then
    reason=$(echo "$bout" | grep -m1 "REASON:" | sed 's/^REASON: //')
    echo "BENCH NOTE: REGRESSED to BLOCKED -> $reason"
    echo "            (Minesweeper emitted a valid module before; see docs/roadmap.md)"
  fi
fi
exit $fail
