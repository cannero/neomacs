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
USE_XVFB="${USE_XVFB:-0}"

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
  --xvfb              Run inside a private Xvfb instead of the current DISPLAY
  -h, --help          Show this help

Environment overrides:
  BIN, LOG, OUTPUT, WINDOW_SIZE, APP, RUST_LOG, USE_XVFB
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
        --xvfb)
            USE_XVFB=1
            shift
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
    echo "build it first with: cargo build -p neomacs" >&2
    exit 1
fi

echo "Running keyboard smoke test with $BIN"
echo "Log: $LOG"
echo "Screenshot: $OUTPUT"

MARKER="$(mktemp "${TMPDIR:-/tmp}/neomacs-keytest.XXXXXX.ok")"
CAPTURE_ARGS=(
    --app "$APP" \
    --bin "$BIN" \
    --no-test-file \
    --log "$LOG" \
    --output "$OUTPUT" \
    --window-size "$WINDOW_SIZE" \
    --wait 120 \
    --after-eval-wait 1 \
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
    --key Return \
    --eval-elisp "(with-temp-file \"$MARKER\" (insert \"keyboard-ok\"))" \
    --wait-file "$MARKER"
)

if [[ "$USE_XVFB" -eq 1 ]]; then
    CAPTURE_ARGS+=(--xvfb)
fi

env RUST_LOG="$RUST_LOG" "$CAPTURE_SCRIPT" "${CAPTURE_ARGS[@]}"

echo
echo "=== Error Scan ==="
if rg -n "entry limit reached|list_keymap_lookup_one|panic|buffer-read-only|invalid-function|unknown key|unhandled" "$LOG"; then
    echo
    echo "keyboard smoke test found runtime errors in $LOG" >&2
    exit 1
else
    echo "no keyboard/runtime errors found"
fi

if [[ ! -f "$MARKER" ]]; then
    echo "keyboard smoke marker was not created: $MARKER" >&2
    exit 1
fi

echo
echo "=== Warning Summary ==="
rg -n " WARN |warning:" "$LOG" | tail -20 || true

echo
echo "Keyboard smoke test completed"
