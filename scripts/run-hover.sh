#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

port="${WLAM_HOVER_PORT:-8765}"
url="http://127.0.0.1:${port}/apps/web/?manifest=/target/hover/bundle/manifest.json"

for dependency in python3 curl; do
  if ! command -v "$dependency" >/dev/null 2>&1; then
    echo "Required command is missing: $dependency" >&2
    exit 1
  fi
done

./scripts/build-hover.sh

if curl --silent --fail "http://127.0.0.1:${port}/apps/web/" >/dev/null 2>&1; then
  echo "Using existing web server at http://127.0.0.1:${port}/"
else
  echo "Starting web server at http://127.0.0.1:${port}/"
  python3 scripts/serve-web.py --port "$port" &
  server_pid=$!
  trap 'kill "$server_pid" 2>/dev/null || true' EXIT INT TERM
  ready=false
  for _ in {1..50}; do
    if curl --silent --fail "http://127.0.0.1:${port}/apps/web/" >/dev/null 2>&1; then
      ready=true
      break
    fi
    if ! kill -0 "$server_pid" 2>/dev/null; then
      echo "Web server exited before it became ready." >&2
      exit 1
    fi
    sleep 0.1
  done
  if [[ "$ready" != true ]]; then
    echo "Web server did not become ready on port $port." >&2
    exit 1
  fi
fi

echo "Opening $url"
echo "Select Start / Restart, then acknowledge Hover's display warning."
if command -v xdg-open >/dev/null 2>&1; then
  xdg-open "$url" >/dev/null 2>&1 || true
elif command -v open >/dev/null 2>&1; then
  open "$url" >/dev/null 2>&1 || true
else
  echo "Open this URL in your browser: $url"
fi

if [[ -n "${server_pid:-}" ]]; then
  wait "$server_pid"
fi
