#!/usr/bin/env bash
# Run the web acceptance tests with this workspace's Playwright install.
#
# The tests import Playwright through WLAM_PLAYWRIGHT_MODULE and look for the
# browser binaries through PLAYWRIGHT_BROWSERS_PATH. In this workspace both can
# live outside the repository (/tmp/wlam-browser-test and /tmp/wlam-playwright-
# browsers), so this script resolves them with fallbacks and leaves any value
# you already exported alone.
#
# Usage:
#   apps/web/tests/run-tests.sh                  # documented browser checks
#   apps/web/tests/run-tests.sh --all            # every .mjs check + Python
#   apps/web/tests/run-tests.sh --python         # Python unittest suite only
#   apps/web/tests/run-tests.sh --serve --all    # start/stop the dev server
#   apps/web/tests/run-tests.sh bundles recompile
#
# Test names are file stems from apps/web/tests/*.mjs. The dev server defaults
# to http://127.0.0.1:8765/apps/web/; override with WLAM_TEST_URL.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
cd "$root"

DEFAULT_TESTS=(browser debugger desktop-window bundles recompile webgl-opengl-fixture)
# guest-worker.mjs is a worker module instantiated by probe-worker.mjs and
# worker-interaction.mjs; it has no standalone entry point and must not be run
# directly. Keep it out of the list.
ALL_TESTS=(
  browser debugger desktop-window bundles recompile
  webgl-opengl-fixture hover-dialog hover-start
  webgl-tolympic webgl-reference-capture probe
  host-adapter opengl-fixture probe-worker worker-interaction
)

serve=0
python=0
all=0
selected=()

for arg in "$@"; do
  case "$arg" in
    --serve) serve=1 ;;
    --python) python=1 ;;
    --all) all=1 ;;
    -h|--help)
      sed -n '2,22p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    -*)
      echo "unknown option: $arg" >&2
      exit 2
      ;;
    *)
      selected+=("${arg%.mjs}")
      ;;
  esac
done

if [[ $all -eq 1 ]]; then
  selected=("${ALL_TESTS[@]}")
elif [[ ${#selected[@]} -eq 0 && $python -eq 0 ]]; then
  selected=("${DEFAULT_TESTS[@]}")
fi

# --- Playwright module -------------------------------------------------------
if [[ -z "${WLAM_PLAYWRIGHT_MODULE:-}" ]]; then
  for candidate in \
    "$root/node_modules/playwright/index.mjs" \
    "$root/apps/web/node_modules/playwright/index.mjs" \
    "/tmp/wlam-browser-test/node_modules/playwright/index.mjs"; do
    if [[ -f "$candidate" ]]; then
      export WLAM_PLAYWRIGHT_MODULE="$candidate"
      break
    fi
  done
fi

# --- Browser binaries --------------------------------------------------------
if [[ -z "${PLAYWRIGHT_BROWSERS_PATH:-}" ]]; then
  for candidate in \
    "${HOME:-/root}/.cache/ms-playwright" \
    "/tmp/wlam-playwright-browsers"; do
    if [[ -d "$candidate" ]]; then
      export PLAYWRIGHT_BROWSERS_PATH="$candidate"
      break
    fi
  done
fi

url="${WLAM_TEST_URL:-http://127.0.0.1:8765/apps/web/}"
server_pid=""

cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

if [[ ${#selected[@]} -gt 0 ]] && ! curl -sf -o /dev/null "$url"; then
  if [[ $serve -eq 1 ]]; then
    echo "Starting dev server for $url"
    mkdir -p /tmp/opencode
    python3 scripts/serve-web.py --port 8765 > /tmp/opencode/serve-web-tests.log 2>&1 &
    server_pid=$!
    for _ in $(seq 1 60); do
      curl -sf -o /dev/null "$url" && break
      sleep 0.5
    done
    if ! curl -sf -o /dev/null "$url"; then
      echo "Dev server did not come up; see /tmp/opencode/serve-web-tests.log" >&2
      exit 1
    fi
  else
    echo "The dev server is not running at $url" >&2
    echo "Start it in another terminal: python3 scripts/serve-web.py --port 8765" >&2
    echo "Or rerun with --serve to start and stop one automatically." >&2
    exit 1
  fi
fi

echo "Playwright module: ${WLAM_PLAYWRIGHT_MODULE:-playwright (bare import)}"
echo "Browser path:      ${PLAYWRIGHT_BROWSERS_PATH:-default}"
echo "Test URL:          $url"
echo

status=0
for name in "${selected[@]}"; do
  file="$here/$name.mjs"
  if [[ ! -f "$file" ]]; then
    echo "missing test: $name.mjs" >&2
    status=1
    continue
  fi
  echo "=== $name ==="
  if ! node "$file"; then
    status=1
    echo "FAILED: $name" >&2
  fi
  echo
done

if [[ $python -eq 1 || $all -eq 1 ]]; then
  echo "=== python unittest ==="
  python3 -m unittest discover -s apps/web/tests -p 'test_*.py' || status=1
fi

exit "$status"
