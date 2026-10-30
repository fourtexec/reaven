#!/usr/bin/env bash
# Runs every automated check. Requires: cargo, xvfb, xdotool, imagemagick,
# python3 (+ websocket-client for the browser check), chromium (optional).
#
#   ./tests/run_all.sh
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
FAIL=0

run() { # run <label> <command...>
  local label=$1; shift
  printf '\n\033[1m=== %s\033[0m\n' "$label"
  if "$@"; then
    printf '\033[32m%s: ok\033[0m\n' "$label"
  else
    printf '\033[31m%s: FAILED\033[0m\n' "$label"
    FAIL=1
  fi
}

printf '\033[1m=== build\033[0m\n'
cargo build || { echo "build failed"; exit 1; }

run "unit tests" cargo test --quiet
run "end-to-end (CLI, Xvfb)" ./tests/e2e.sh "$ROOT/target/debug/reaven"
run "android client protocol" xvfb-run -a -s "-screen 0 1440x900x24" \
    python3 tests/android_client_sim.py "$ROOT/target/debug/reaven"

if command -v chromium >/dev/null && python3 -c "import websocket" 2>/dev/null; then
  printf '\n\033[1m=== viewer in a real browser\033[0m\n'
  export REAVEN_PASSWORD=browser-check
  PORT=${REAVEN_BROWSER_PORT:-7465}
  Xvfb :88 -screen 0 1200x700x24 >/dev/null 2>&1 & XPID=$!
  sleep 1
  DISPLAY=:88 ./target/debug/reaven server --listen "127.0.0.1:$PORT" \
      --password "$REAVEN_PASSWORD" >/dev/null 2>&1 & RPID=$!
  sleep 0.7
  DISPLAY=:88 ./target/debug/reaven connect --server "127.0.0.1:$PORT" --name pc \
      >/dev/null 2>&1 & APID=$!
  sleep 1.2
  DISPLAY=:88 ./target/debug/reaven screen --server "127.0.0.1:$PORT" --target pc \
      --bind 127.0.0.1:8090 --fps 15 >/dev/null 2>&1 & VPID=$!
  sleep 2.5
  if DISPLAY=:88 python3 tests/viewer_browser_check.py http://127.0.0.1:8090/; then
    printf '\033[32mviewer browser check: ok\033[0m\n'
  else
    printf '\033[31mviewer browser check: FAILED\033[0m\n'
    FAIL=1
  fi
  kill $VPID $APID $RPID $XPID 2>/dev/null
  wait 2>/dev/null
else
  printf '\n(viewer browser check skipped: needs chromium + python websocket-client)\n'
fi

printf '\n'
if [ "$FAIL" -eq 0 ]; then
  printf '\033[32m================ all checks passed ================\033[0m\n'
else
  printf '\033[31m================ some checks failed ================\033[0m\n'
fi
exit "$FAIL"
