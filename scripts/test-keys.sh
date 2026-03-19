#!/bin/bash
# Test keyboard input in neomacs using xdotool
# Usage: ./scripts/test-keys.sh [log_file]

LOG="${1:-/tmp/neomacs-keytest.log}"
RUST_LOG="${RUST_LOG:-info}"

echo "Starting neomacs -Q with RUST_LOG=$RUST_LOG..."
RUST_LOG=$RUST_LOG ./target/release/neomacs -Q > "$LOG" 2>&1 &
PID=$!

# Wait for window to appear
echo "Waiting for Neomacs window..."
for i in $(seq 1 20); do
  WID=$(xdotool search --name "Neomacs" 2>/dev/null | head -1)
  if [ -n "$WID" ]; then
    echo "Found window: $WID (after ${i}s)"
    break
  fi
  sleep 1
done

if [ -z "$WID" ]; then
  echo "ERROR: Neomacs window not found after 20s"
  kill $PID 2>/dev/null
  exit 1
fi

# Focus and activate
xdotool windowactivate --sync $WID
sleep 0.5

# Send test keys
echo "Sending: j k BackSpace M-x"
xdotool key j
sleep 0.5
xdotool key k
sleep 0.5
xdotool key BackSpace
sleep 0.5
xdotool key alt+x
sleep 2

# Capture screenshot
import -window root /tmp/neomacs-keytest.png 2>/dev/null

# Kill neomacs
kill $PID 2>/dev/null
wait $PID 2>/dev/null

echo ""
echo "=== Key dispatch results ==="
grep -a "command_loop_1.*binding=\|Undefined key\|is undefined\|read_char.*received\|Command error" "$LOG" | tail -20
echo ""
echo "=== Warnings ==="
grep -a "WARN" "$LOG" | tail -5
echo ""
echo "Log: $LOG"
echo "Screenshot: /tmp/neomacs-keytest.png"
