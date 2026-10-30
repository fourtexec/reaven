#!/usr/bin/env bash
# Runs the whole stack locally against a virtual display, so you can see the
# viewer and the control path without a second machine or a real desktop session.
#
#   ./scripts/demo.sh            # relay + agent on Xvfb, viewer on :8080
#   ./scripts/demo.sh --keep     # leave everything running
#
# Open http://127.0.0.1:8080/ once it prints the URL.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=$ROOT/target/release/reaven
KEEP=0
[ "${1:-}" = "--keep" ] && KEEP=1

[ -x "$BIN" ] || { echo "build first: cargo build --release"; exit 1; }
command -v Xvfb >/dev/null || { echo "install xvfb to run the demo"; exit 1; }

PORT=${REAVEN_DEMO_PORT:-7431}
PASS=demo-password
export REAVEN_PASSWORD=$PASS

cleanup() {
  [ "$KEEP" = "1" ] && return
  kill "${PIDS[@]}" 2>/dev/null || true
}
PIDS=()
trap cleanup EXIT

echo "→ starting Xvfb"
Xvfb :91 -screen 0 1600x900x24 >/dev/null 2>&1 &
PIDS+=($!)
export DISPLAY=:91
sleep 1

# Something worth looking at in the stream.
if command -v xterm >/dev/null; then
  xterm -title "demo: this is the remote desktop" -geometry 80x20+40+40 &
  PIDS+=($!)
fi

echo "→ starting relay on 127.0.0.1:$PORT"
"$BIN" server --listen "127.0.0.1:$PORT" --password "$PASS" &
PIDS+=($!)
sleep 0.7

echo "→ starting agent 'demo-pc'"
"$BIN" connect --server "127.0.0.1:$PORT" --name demo-pc &
PIDS+=($!)
sleep 1

echo "→ online machines:"
"$BIN" peers --server "127.0.0.1:$PORT"

echo
echo "Try, in another shell:"
echo "  export REAVEN_PASSWORD=$PASS"
echo "  reaven screenshot --server 127.0.0.1:$PORT --target demo-pc --output demo.jpg"
echo "  reaven input --server 127.0.0.1:$PORT --target demo-pc --at 0.5,0.5 --click left"
echo

# Foreground: the streaming viewer (Ctrl-C to stop).
exec "$BIN" screen --server "127.0.0.1:$PORT" --target demo-pc \
  --fps 12 --quality 70 --max-width 1600 --bind 127.0.0.1:8080
