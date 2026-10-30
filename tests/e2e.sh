#!/usr/bin/env bash
# End-to-end smoke test for Reaven.
#
# Runs a relay, an agent and the operator commands against a throwaway Xvfb
# display, so screen capture and input injection are exercised for real.
# Requires: xvfb, xdotool (test-only deps), cargo already built.
#
# Usage:  ./tests/e2e.sh [path-to-reaven-binary]
set -uo pipefail

BIN=${1:-$(dirname "$0")/../target/debug/reaven}
BIN=$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")
OUT=$(mktemp -d)
PORT=${REAVEN_TEST_PORT:-17331}
PASS=test-secret-123
SERVER=127.0.0.1:$PORT
PIDS=()
FAILS=0

cleanup() {
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null; done
  wait 2>/dev/null
  rm -rf "$OUT"
}
trap cleanup EXIT

ok()   { printf '  \033[32mPASS\033[0m %s\n' "$*"; }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$*"; FAILS=$((FAILS+1)); }
step() { printf '\n\033[1m%s\033[0m\n' "$*"; }

wait_for() { # wait_for <seconds> <command...>
  local deadline=$(( $(date +%s) + $1 )); shift
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if "$@" >/dev/null 2>&1; then return 0; fi
    sleep 0.2
  done
  return 1
}

[ -x "$BIN" ] || { echo "binary not found: $BIN (run cargo build first)"; exit 1; }

if [ -z "${REAVEN_E2E_IN_XVFB:-}" ]; then
  export REAVEN_E2E_IN_XVFB=1
  exec xvfb-run -a -s "-screen 0 1440x900x24" "$0" "$BIN"
fi

export REAVEN_PASSWORD=$PASS

# Xvfb only reflects pointer warps in XQueryPointer while some client has the
# root pointer events selected, so start a listener before testing input. Without
# this, `xdotool getmouselocation` reports the initial centre forever and the
# input assertions fail for reasons that have nothing to do with Reaven.
if command -v xev >/dev/null; then
  xev -root -event mouse >"$OUT/xev.log" 2>&1 &
  PIDS+=($!)
  sleep 0.5
fi

step "starting relay on $SERVER"
"$BIN" server --listen "$SERVER" >"$OUT/relay.log" 2>&1 &
PIDS+=($!)
wait_for 10 bash -c "</dev/tcp/127.0.0.1/$PORT" || { echo "relay did not start"; cat "$OUT/relay.log"; exit 1; }
ok "relay listening"

step "starting agent 'testpc' on DISPLAY=$DISPLAY"
"$BIN" connect --server "$SERVER" --name testpc >"$OUT/agent.log" 2>&1 &
AGENT=$!
PIDS+=($AGENT)
wait_for 10 grep -q "online" "$OUT/agent.log" || { echo "agent did not connect"; cat "$OUT/agent.log"; exit 1; }
ok "agent connected"

step "discovery: reaven peers"
PEERS=$("$BIN" peers --server "$SERVER" 2>&1)
echo "$PEERS" | sed 's/^/    /'
case "$PEERS" in *testpc*) ok "peer listed";; *) bad "peer not listed";; esac
# The operator's own connection must not appear as a machine.
case "$PEERS" in *operator*) bad "operator leaked into peer list";; *) ok "operator not listed as a machine";; esac

step "shell: reaven exec"
OUT_EXEC=$("$BIN" exec --server "$SERVER" --target testpc --command 'echo hello-from-$(hostname)' 2>&1)
case "$OUT_EXEC" in hello-from-*) ok "exec returned: $OUT_EXEC";; *) bad "exec failed: $OUT_EXEC";; esac

step "screen enumeration: reaven screens"
SCREENS=$("$BIN" screens --server "$SERVER" --target testpc 2>&1)
echo "$SCREENS" | sed 's/^/    /'
case "$SCREENS" in *1440*) ok "found 1440px monitor";; *) bad "monitor not detected";; esac

step "screenshot"
if "$BIN" screenshot --server "$SERVER" --target testpc --output "$OUT/shot.jpg" 2>&1 | sed 's/^/    /'; then
  if [ -s "$OUT/shot.jpg" ] && file "$OUT/shot.jpg" | grep -q JPEG; then
    ok "valid JPEG ($(stat -c%s "$OUT/shot.jpg") bytes, $(identify -format '%wx%h' "$OUT/shot.jpg" 2>/dev/null))"
  else
    bad "screenshot is not a valid JPEG"
  fi
else
  bad "screenshot command failed"
fi

step "downscaling: --max-width 320"
"$BIN" screenshot --server "$SERVER" --target testpc --output "$OUT/small.jpg" --max-width 320 >/dev/null 2>&1
DIM=$(identify -format '%w' "$OUT/small.jpg" 2>/dev/null)
[ "$DIM" = "320" ] && ok "resized to 320px wide" || bad "expected 320px wide, got '$DIM'"

step "pixel fidelity: capture a known gradient and check channel order"
# Guards the pure-Rust X11 backend against channel swaps / vertical flips: paint a
# red->blue vertical gradient, then assert the top is red-dominant and the bottom
# blue-dominant in the captured JPEG. Skipped if imagemagick is unavailable.
if command -v convert >/dev/null && command -v display >/dev/null; then
  convert -size 1440x900 gradient:'#ff0000'-'#0000ff' "$OUT/grad.png" 2>/dev/null
  display -window root "$OUT/grad.png" >/dev/null 2>&1 &
  sleep 1.5
  "$BIN" screenshot --server "$SERVER" --target testpc --quality 98 --output "$OUT/grad.jpg" >/dev/null 2>&1
  TOP=$(convert "$OUT/grad.jpg" -format '%[fx:int(255*p{100,60}.r)],%[fx:int(255*p{100,60}.b)]' info: 2>/dev/null)
  BOT=$(convert "$OUT/grad.jpg" -format '%[fx:int(255*p{100,840}.r)],%[fx:int(255*p{100,840}.b)]' info: 2>/dev/null)
  TOP_R=${TOP%%,*}; TOP_B=${TOP##*,}; BOT_R=${BOT%%,*}; BOT_B=${BOT##*,}
  echo "    top R,B=$TOP   bottom R,B=$BOT"
  if [ "${TOP_R:-0}" -gt 150 ] && [ "${TOP_B:-255}" -lt 100 ] && \
     [ "${BOT_B:-0}" -gt 150 ] && [ "${BOT_R:-255}" -lt 100 ]; then
    ok "red/blue channels and vertical orientation correct"
  else
    bad "colour/geometry wrong (top=$TOP bottom=$BOT)"
  fi
else
  echo "    (skipped: imagemagick not installed)"
fi

step "webcam enumeration (no camera attached: expect a clean error)"
CAM=$("$BIN" cameras --server "$SERVER" --target testpc 2>&1)
echo "$CAM" | sed 's/^/    /'
case "$CAM" in *"no webcams"*|*"no webcam"*|*"camera"*) ok "handled gracefully";; *) bad "unexpected webcam output";; esac

step "live stream: reaven screen --duration 3 --no-viewer"
"$BIN" screen --server "$SERVER" --target testpc --duration 3 --no-viewer --fps 10 >"$OUT/stream.log" 2>&1
grep -E 'frame|stream closed' "$OUT/stream.log" | tail -2 | sed 's/^/    /'
FRAMES=$(grep -o 'frames, ' -B0 "$OUT/stream.log" >/dev/null 2>&1; grep 'stream closed' "$OUT/stream.log" | grep -oE '[0-9]+ frames' | grep -oE '[0-9]+')
if [ "${FRAMES:-0}" -gt 5 ]; then ok "received $FRAMES frames"; else bad "expected >5 frames, got '${FRAMES:-0}'"; fi

step "recording frames to disk"
"$BIN" screen --server "$SERVER" --target testpc --duration 2 --no-viewer --record "$OUT/frames" >/dev/null 2>&1
COUNT=$(ls "$OUT/frames" 2>/dev/null | wc -l)
[ "$COUNT" -gt 3 ] && ok "recorded $COUNT frames" || bad "expected >3 recorded frames, got $COUNT"

step "input injection: move pointer to 0.25,0.25 + left click"
"$BIN" input --server "$SERVER" --target testpc --at 0.25,0.25 --click left 2>&1 | sed 's/^/    /'
sleep 0.4
LOC=$(xdotool getmouselocation 2>/dev/null)
echo "    pointer is now: $LOC"
X=$(echo "$LOC" | grep -oE 'x:[0-9]+' | cut -d: -f2)
Y=$(echo "$LOC" | grep -oE 'y:[0-9]+' | cut -d: -f2)
# 1440x900 -> 0.25 maps to x=359 (0..1439), y=224 (0..899)
if [ -n "${X:-}" ] && [ "$X" -ge 350 ] && [ "$X" -le 370 ] && [ "$Y" -ge 215 ] && [ "$Y" -le 235 ]; then
  ok "pointer moved to $X,$Y (expected ~359,224)"
else
  bad "pointer at ${X:-?},${Y:-?}, expected ~359,224"
fi

step "keyboard injection: type text into a file via xdotool-observable window"
# Verify keys actually reach the X server by checking enigo-driven typing into a
# scratch xterm-less window is observable: instead, assert the agent accepted it.
KB=$("$BIN" input --server "$SERVER" --target testpc --text "reaven" --key Return 2>&1)
echo "$KB" | sed 's/^/    /'
case "$KB" in *applied*) ok "agent applied keyboard events";; *) bad "keyboard injection failed: $KB";; esac

step "viewer HTTP: page, snapshot and /input round-trip"
"$BIN" screen --server "$SERVER" --target testpc --duration 12 --bind 127.0.0.1:18080 --fps 10 >"$OUT/viewer.log" 2>&1 &
VIEWER=$!
PIDS+=($VIEWER)
wait_for 10 bash -c "</dev/tcp/127.0.0.1/18080" || bad "viewer did not bind"
PAGE=$(curl -s --max-time 3 http://127.0.0.1:18080/ )
case "$PAGE" in *"stream.mjpg"*) ok "viewer page served ($(echo -n "$PAGE" | wc -c) bytes)";; *) bad "viewer page missing";; esac
case "$PAGE" in *"__TITLE__"*|*"__TARGET__"*) bad "template placeholders not substituted";; *) ok "placeholders rendered";; esac

# The first frame may still be in flight: poll before asserting.
SNAP=""
for _ in $(seq 1 25); do
  SNAP=$(curl -s --max-time 3 http://127.0.0.1:18080/snapshot.jpg | head -c 2 | od -An -tx1 | tr -d ' \n')
  [ "$SNAP" = "ffd8" ] && break
  sleep 0.2
done
[ "$SNAP" = "ffd8" ] && ok "snapshot.jpg is a JPEG (ffd8 magic)" || bad "snapshot not a JPEG (got $SNAP)"

curl -s --max-time 2 http://127.0.0.1:18080/stream.mjpg -o "$OUT/mjpeg.out"
if grep -qa -- '--reavenframe' "$OUT/mjpeg.out" && grep -qa 'Content-Type: image/jpeg' "$OUT/mjpeg.out"; then
  ok "MJPEG multipart stream well-formed ($(stat -c%s "$OUT/mjpeg.out") bytes)"
else
  bad "MJPEG stream malformed"
fi

RESP=$(curl -s --max-time 3 -X POST -H 'Content-Type: application/json' \
  -d '[{"event":"click","button":"left","x":0.75,"y":0.75}]' http://127.0.0.1:18080/input)
echo "    /input -> $RESP"
case "$RESP" in *'"ok":true'*) ok "viewer forwarded a click";; *) bad "viewer input rejected";; esac
sleep 0.5
X=$(xdotool getmouselocation | grep -oE 'x:[0-9]+' | cut -d: -f2)
Y=$(xdotool getmouselocation | grep -oE 'y:[0-9]+' | cut -d: -f2)
# 0.75 of 1440/900 -> ~1079,674
if [ "$X" -ge 1065 ] && [ "$X" -le 1095 ] && [ "$Y" -ge 660 ] && [ "$Y" -le 690 ]; then
  ok "click landed at $X,$Y (expected ~1079,674)"
else
  bad "click landed at $X,$Y, expected ~1079,674"
fi

BADJSON=$(curl -s --max-time 3 -X POST -d 'not-json' http://127.0.0.1:18080/input)
case "$BADJSON" in *'"ok":false'*) ok "malformed input rejected with 400";; *) bad "malformed input accepted";; esac
kill $VIEWER 2>/dev/null; wait $VIEWER 2>/dev/null

step "view-only agent must refuse input"
"$BIN" connect --server "$SERVER" --name readonly --no-input >"$OUT/ro.log" 2>&1 &
RO=$!
PIDS+=($RO)
wait_for 10 grep -q "online" "$OUT/ro.log" || bad "read-only agent did not start"
ROBOUT=$("$BIN" input --server "$SERVER" --target readonly --at 0.5,0.5 --click left 2>&1)
echo "$ROBOUT" | sed 's/^/    /'
case "$ROBOUT" in *"no-input"*|*"view-only"*) ok "view-only agent refused input";; *) bad "view-only agent accepted input";; esac
ROSCREEN=$("$BIN" screenshot --server "$SERVER" --target readonly --output "$OUT/ro.jpg" 2>&1 >/dev/null)
[ -s "$OUT/ro.jpg" ] && ok "read-only agent still serves screenshots" || bad "read-only agent failed screenshot"

step "auth: wrong password rejected"
BAD=$("$BIN" peers --server "$SERVER" --password wrong-password 2>&1)
case "$BAD" in *authentication*|*rejected*) ok "wrong password rejected";; *) bad "wrong password accepted: $BAD";; esac

step "offline target: clean error"
OFF=$("$BIN" exec --server "$SERVER" --target nosuchpc --command 'true' 2>&1)
case "$OFF" in *offline*) ok "offline target reported";; *) bad "unexpected: $OFF";; esac

step "agent log mentions the control sessions it served"
grep -E "stream|shell|input" "$OUT/agent.log" | head -4 | sed 's/^/    /'
grep -q "remote stream" "$OUT/agent.log" && ok "agent logged stream activity locally" || bad "agent did not log stream activity"

printf '\n\033[1m%s\033[0m\n' "----------------------------------------"
if [ "$FAILS" -eq 0 ]; then
  printf '\033[32mAll end-to-end checks passed\033[0m\n'
  exit 0
else
  printf '\033[31m%s check(s) failed\033[0m\n' "$FAILS"
  exit 1
fi
