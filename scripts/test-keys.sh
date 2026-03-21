#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CAPTURE_SCRIPT="$ROOT_DIR/scripts/capture-face-test.sh"

BIN="${BIN:-$ROOT_DIR/target/debug/neomacs}"
LOG="${LOG:-/tmp/neomacs-keytest.log}"
OUTPUT="${OUTPUT:-/tmp/neomacs-keytest.png}"
RUST_LOG="${RUST_LOG:-debug}"
WINDOW_SIZE="${WINDOW_SIZE:-1400x1000}"
APP="${APP:-neomacs}"

usage() {
    cat <<EOF
Usage: $0 [options]

Keyboard smoke test for Neomacs/Emacs using xdotool through capture-face-test.sh.

Options:
  --bin PATH          Editor binary (default: $BIN)
  --log PATH          Log file (default: $LOG)
  --output PATH       Screenshot path (default: $OUTPUT)
  --window-size WxH   Window size (default: $WINDOW_SIZE)
  --app NAME          App hint for capture helper (default: $APP)
  -h, --help          Show this help

Environment overrides:
  BIN, LOG, OUTPUT, WINDOW_SIZE, APP, RUST_LOG
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --bin)
            BIN="$2"
            shift 2
            ;;
        --log)
            LOG="$2"
            shift 2
            ;;
        --output)
            OUTPUT="$2"
            shift 2
            ;;
        --window-size)
            WINDOW_SIZE="$2"
            shift 2
            ;;
        --app)
            APP="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ ! -x "$CAPTURE_SCRIPT" ]]; then
    echo "missing capture helper: $CAPTURE_SCRIPT" >&2
    exit 1
fi

if [[ ! -x "$BIN" ]]; then
    echo "missing editor binary: $BIN" >&2
    echo "build it first with: cargo build -p neomacs-bin" >&2
    exit 1
fi

echo "Running keyboard smoke test with $BIN"
echo "Log: $LOG"
echo "Screenshot: $OUTPUT"

env RUST_LOG="$RUST_LOG" "$CAPTURE_SCRIPT" \
    --app "$APP" \
    --bin "$BIN" \
    --no-test-file \
    --xvfb \
    --log "$LOG" \
    --output "$OUTPUT" \
    --window-size "$WINDOW_SIZE" \
    --wait 120 \
    --after-ready-wait 1 \
    --type "hello keyboard path" \
    --key Return \
    --key ctrl+x \
    --key 2 \
    --key ctrl+x \
    --key o \
    --type "other pane text" \
    --key alt+x \
    --type "list-buffers" \
    --key Return \
    --key ctrl+x \
    --key b \
    --type "*Messages*" \
    --key Return

echo
echo "=== Error Scan ==="
if rg -n "entry limit reached|list_keymap_lookup_one|Command error|panic|buffer-read-only|unknown key|unhandled" "$LOG"; then
    echo
    echo "keyboard smoke test found runtime errors in $LOG" >&2
    exit 1
else
    echo "no keyboard/runtime errors found"
fi

echo
echo "=== Warning Summary ==="
rg -n " WARN |warning:" "$LOG" | tail -20 || true

echo
echo "Keyboard smoke test completed"
