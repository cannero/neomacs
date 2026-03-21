#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EMACS_BIN_DEFAULT="/home/exec/Projects/github.com/emacs-mirror/emacs/src/emacs"
NEOMACS_BIN_RELEASE="$ROOT_DIR/target/release/neomacs"
NEOMACS_BIN_DEBUG="$ROOT_DIR/target/debug/neomacs"
TEST_FILE_DEFAULT="$ROOT_DIR/test/neomacs/neomacs-face-test.el"

APP="neomacs"
BIN_OVERRIDE=""
OUTPUT=""
LOG_FILE=""
WAIT_SECONDS=180
POST_WAIT=1
AFTER_RESIZE_WAIT=0
AFTER_EVAL_WAIT=0
AFTER_READY_WAIT=0
KEEP_RUNNING=0
RUST_LOG_VALUE="${RUST_LOG:-debug}"
WINDOW_SIZE=""
START_XVFB=0
XVFB_DISPLAY="auto"
XVFB_SCREEN="1600x1800x24"
XVFB_WAIT=10
XVFB_LOG=""
EXTRA_ARGS=()
LOAD_FILES=()
KEYS=()
TYPES=()
EVAL_ELISP=()
ACTION_KINDS=()
ACTION_VALUES=()
STARTUP_EVALS=()
KEY_DELAY=0.3
TYPE_DELAY_MS=0
AUTO_REPORT_FILE=""
AUTO_REPORT_DELAY="0.5"
AUTO_REPORT_EXPECTED_FRAME_SIZE=""
WAIT_FOR_FILE=""
WAIT_FOR_LOG_PATTERN=""
WAIT_FOR_LOG_BEFORE_RESIZE=""
READY_TIMEOUT=""
WINDOW_SIZE_WIDTH=""
WINDOW_SIZE_HEIGHT=""
AUTO_REPORT_EXPECTED_FRAME_WIDTH=""
AUTO_REPORT_EXPECTED_FRAME_HEIGHT=""

choose_default_neomacs_bin() {
    if [[ -x "$NEOMACS_BIN_DEBUG" ]]; then
        printf '%s\n' "$NEOMACS_BIN_DEBUG"
    else
        printf '%s\n' "$NEOMACS_BIN_RELEASE"
    fi
}

usage() {
    cat <<'EOF'
Usage: capture-face-test.sh [options]

Launch Neomacs or GNU Emacs on the face test file, wait for a visible X11
window, optionally send keys, and capture a screenshot.

Options:
  --app APP            neomacs or emacs (default: neomacs)
  --bin PATH           Override the editor binary path
  --output FILE        Screenshot output path
  --log FILE           Log output path
  --wait SECONDS       Seconds to wait for a visible window (default: 180)
  --post-wait SECONDS  Extra delay after the window appears (default: 1)
  --after-resize-wait SECONDS
                       Extra delay after an X11 resize before input/capture
  --after-eval-wait SECONDS
                       Extra delay after each M-: eval before capture
  --after-ready-wait SECONDS
                       Extra delay after wait-file/wait-log succeeds
  --window-size WxH    Resize the X11 window before capture
  --xvfb               Start a private Xvfb display for this run
  --xvfb-display DISP  Xvfb display number or 'auto' (default: auto)
  --xvfb-screen SPEC   Xvfb screen spec (default: 1600x1800x24)
  --xvfb-wait SEC      Seconds to wait for Xvfb to accept clients (default: 10)
  --keep-running       Leave the editor alive after capture
  --key KEY            Send an xdotool key after focusing the window
  --type TEXT          Send literal text to the target window (repeatable)
  --type-delay MS      Milliseconds between characters for xdotool type (default: 0)
  --eval-elisp EXPR    Evaluate Elisp via M-: after resize/focus (repeatable)
  --startup-eval EXPR  Pass --eval EXPR on the editor command line (repeatable)
  --load FILE          Load an extra Elisp file after the face test (repeatable)
  --auto-report FILE   Load the helper and write the face matrix report to FILE
  --auto-report-delay SEC
                       Delay before helper writes the report (default: 0.5)
  --auto-report-frame-size WxH
                       Require this Emacs frame-pixel size before writing the
                       auto-report (disabled by default)
  --wait-file FILE     Wait until FILE exists before capture
  --wait-log REGEX     Wait until REGEX matches the log before capture
  --wait-log-before-resize REGEX
                       Wait until REGEX matches the log before resizing/input
  --ready-timeout SEC  Timeout for wait-file/wait-log (default: --wait)
  --key-delay SEC      Delay between xdotool inputs (default: 0.3)
  --rust-log LEVEL     RUST_LOG value for Neomacs (default: current RUST_LOG or debug)
  --arg ARG            Extra editor argument (repeatable)
  -h, --help           Show this help

Examples:
  ./scripts/capture-face-test.sh --app neomacs --output /tmp/neomacs-face.png
  ./scripts/capture-face-test.sh --app emacs --key Next --output /tmp/emacs-face-page2.png
  ./scripts/capture-face-test.sh --app neomacs --key ctrl+s --type "UNDERLINE (5 styles)" --key Return
  ./scripts/capture-face-test.sh --app neomacs --window-size 1400x1600 --startup-eval "(neomacs-face-test-write-matrix-report \"/tmp/report.txt\")"
  ./scripts/capture-face-test.sh --app neomacs --auto-report /tmp/report.txt --output /tmp/face.png
EOF
}

die() {
    echo "ERROR: $*" >&2
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --app)
            APP="$2"
            shift 2
            ;;
        --output)
            OUTPUT="$2"
            shift 2
            ;;
        --bin)
            BIN_OVERRIDE="$2"
            shift 2
            ;;
        --log)
            LOG_FILE="$2"
            shift 2
            ;;
        --wait)
            WAIT_SECONDS="$2"
            shift 2
            ;;
        --post-wait)
            POST_WAIT="$2"
            shift 2
            ;;
        --after-resize-wait)
            AFTER_RESIZE_WAIT="$2"
            shift 2
            ;;
        --after-eval-wait)
            AFTER_EVAL_WAIT="$2"
            shift 2
            ;;
        --after-ready-wait)
            AFTER_READY_WAIT="$2"
            shift 2
            ;;
        --window-size)
            WINDOW_SIZE="$2"
            shift 2
            ;;
        --xvfb)
            START_XVFB=1
            shift
            ;;
        --xvfb-display)
            XVFB_DISPLAY="$2"
            shift 2
            ;;
        --xvfb-screen)
            XVFB_SCREEN="$2"
            shift 2
            ;;
        --xvfb-wait)
            XVFB_WAIT="$2"
            shift 2
            ;;
        --keep-running)
            KEEP_RUNNING=1
            shift
            ;;
        --key)
            KEYS+=("$2")
            ACTION_KINDS+=("key")
            ACTION_VALUES+=("$2")
            shift 2
            ;;
        --type)
            TYPES+=("$2")
            ACTION_KINDS+=("type")
            ACTION_VALUES+=("$2")
            shift 2
            ;;
        --type-delay)
            TYPE_DELAY_MS="$2"
            shift 2
            ;;
        --eval-elisp)
            EVAL_ELISP+=("$2")
            ACTION_KINDS+=("eval")
            ACTION_VALUES+=("$2")
            shift 2
            ;;
        --startup-eval)
            STARTUP_EVALS+=("$2")
            shift 2
            ;;
        --load)
            LOAD_FILES+=("$2")
            shift 2
            ;;
        --auto-report)
            AUTO_REPORT_FILE="$2"
            shift 2
            ;;
        --auto-report-delay)
            AUTO_REPORT_DELAY="$2"
            shift 2
            ;;
        --auto-report-frame-size)
            AUTO_REPORT_EXPECTED_FRAME_SIZE="$2"
            shift 2
            ;;
        --wait-file)
            WAIT_FOR_FILE="$2"
            shift 2
            ;;
        --wait-log)
            WAIT_FOR_LOG_PATTERN="$2"
            shift 2
            ;;
        --wait-log-before-resize)
            WAIT_FOR_LOG_BEFORE_RESIZE="$2"
            shift 2
            ;;
        --ready-timeout)
            READY_TIMEOUT="$2"
            shift 2
            ;;
        --key-delay)
            KEY_DELAY="$2"
            shift 2
            ;;
        --rust-log)
            RUST_LOG_VALUE="$2"
            shift 2
            ;;
        --arg)
            EXTRA_ARGS+=("$2")
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown option: $1"
            ;;
    esac
done

command -v xdotool >/dev/null || die "xdotool is required"
command -v import >/dev/null || die "ImageMagick import is required"
if [[ "$START_XVFB" -eq 0 ]]; then
    [[ -n "${DISPLAY:-}" ]] || die "DISPLAY is not set"
fi

if [[ -n "$WINDOW_SIZE" ]]; then
    WINDOW_SIZE_WIDTH="${WINDOW_SIZE%x*}"
    WINDOW_SIZE_HEIGHT="${WINDOW_SIZE#*x}"
    [[ "$WINDOW_SIZE_WIDTH" =~ ^[0-9]+$ && "$WINDOW_SIZE_HEIGHT" =~ ^[0-9]+$ ]] \
        || die "invalid --window-size: $WINDOW_SIZE"
fi

if [[ -n "$AUTO_REPORT_EXPECTED_FRAME_SIZE" ]]; then
    AUTO_REPORT_EXPECTED_FRAME_WIDTH="${AUTO_REPORT_EXPECTED_FRAME_SIZE%x*}"
    AUTO_REPORT_EXPECTED_FRAME_HEIGHT="${AUTO_REPORT_EXPECTED_FRAME_SIZE#*x}"
    [[ "$AUTO_REPORT_EXPECTED_FRAME_WIDTH" =~ ^[0-9]+$ && "$AUTO_REPORT_EXPECTED_FRAME_HEIGHT" =~ ^[0-9]+$ ]] \
        || die "invalid --auto-report-frame-size: $AUTO_REPORT_EXPECTED_FRAME_SIZE"
fi

choose_xvfb_display() {
    local display
    for display in {99..140}; do
        if [[ ! -e "/tmp/.X11-unix/X${display}" && ! -e "/tmp/.X${display}-lock" ]]; then
            printf ':%s\n' "$display"
            return 0
        fi
    done
    die "failed to find a free Xvfb display"
}

wait_for_x_server() {
    local deadline=$((SECONDS + XVFB_WAIT))
    while (( SECONDS <= deadline )); do
        if DISPLAY="$DISPLAY" xdpyinfo >/dev/null 2>&1; then
            return 0
        fi
        if ! kill -0 "$XVFB_PID" 2>/dev/null; then
            die "Xvfb exited before accepting clients"
        fi
        sleep 1
    done
    die "timed out waiting for Xvfb on $DISPLAY"
}

if [[ -z "$READY_TIMEOUT" ]]; then
    READY_TIMEOUT="$WAIT_SECONDS"
fi

case "$APP" in
    neomacs)
        BIN="$(choose_default_neomacs_bin)"
        TITLE_HINT="Neomacs"
        DEFAULT_OUTPUT="/tmp/neomacs-face-test.png"
        DEFAULT_LOG="/tmp/neomacs-face-test.log"
        CMD=("$BIN" -Q -l "$TEST_FILE_DEFAULT")
        ;;
    emacs)
        BIN="$EMACS_BIN_DEFAULT"
        TITLE_HINT="Emacs"
        DEFAULT_OUTPUT="/tmp/emacs-face-test.png"
        DEFAULT_LOG="/tmp/emacs-face-test.log"
        CMD=("$BIN" -Q -l "$TEST_FILE_DEFAULT")
        ;;
    *)
        die "unsupported app: $APP"
        ;;
esac

if [[ -n "$BIN_OVERRIDE" ]]; then
    BIN="$BIN_OVERRIDE"
fi

[[ -x "$BIN" ]] || die "binary not found or not executable: $BIN"

if [[ -z "$OUTPUT" ]]; then
    OUTPUT="$DEFAULT_OUTPUT"
fi
if [[ -z "$LOG_FILE" ]]; then
    LOG_FILE="$DEFAULT_LOG"
fi

CMD+=("${EXTRA_ARGS[@]}")
for load_file in "${LOAD_FILES[@]}"; do
    CMD+=(-l "$load_file")
done
if [[ -n "$AUTO_REPORT_FILE" ]]; then
    AUTO_REPORT_ESCAPED="${AUTO_REPORT_FILE//\\/\\\\}"
    AUTO_REPORT_ESCAPED="${AUTO_REPORT_ESCAPED//\"/\\\"}"
    CMD+=(
        -l "$ROOT_DIR/scripts/face-test-autoreport.el"
        --eval "(setq neomacs-face-test-autoreport-file \"$AUTO_REPORT_ESCAPED\")"
        --eval "(setq neomacs-face-test-autoreport-delay $AUTO_REPORT_DELAY)"
        --eval "(setq neomacs-face-test-autoreport-timeout $READY_TIMEOUT)"
    )
    if [[ -n "$AUTO_REPORT_EXPECTED_FRAME_WIDTH" && -n "$AUTO_REPORT_EXPECTED_FRAME_HEIGHT" ]]; then
        CMD+=(
            --eval "(setq neomacs-face-test-autoreport-expected-frame-width $AUTO_REPORT_EXPECTED_FRAME_WIDTH)"
            --eval "(setq neomacs-face-test-autoreport-expected-frame-height $AUTO_REPORT_EXPECTED_FRAME_HEIGHT)"
        )
    fi
    CMD+=(--eval "(neomacs-face-test-autoreport-arm)")
    if [[ -z "$WAIT_FOR_FILE" ]]; then
        WAIT_FOR_FILE="$AUTO_REPORT_FILE"
    fi
fi
for expr in "${STARTUP_EVALS[@]}"; do
    CMD+=(--eval "$expr")
done

RUN_STAMP="$(mktemp "${TMPDIR:-/tmp}/capture-face-test.XXXXXX")"
XVFB_PID=""

wait_for_path() {
    local target="$1"
    local deadline=$((SECONDS + READY_TIMEOUT))
    while (( SECONDS <= deadline )); do
        if [[ -e "$target" && "$target" -nt "$RUN_STAMP" ]]; then
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            tail -n 120 "$LOG_FILE" >&2 || true
            die "$APP exited before ready file appeared for this run: $target"
        fi
        sleep 1
    done
    tail -n 120 "$LOG_FILE" >&2 || true
    die "timed out waiting for file from this run: $target"
}

wait_for_log_pattern() {
    local pattern="$1"
    local deadline=$((SECONDS + READY_TIMEOUT))
    while (( SECONDS <= deadline )); do
        if grep -Eq -- "$pattern" "$LOG_FILE"; then
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            tail -n 120 "$LOG_FILE" >&2 || true
            die "$APP exited before log matched regex: $pattern"
        fi
        sleep 1
    done
    tail -n 120 "$LOG_FILE" >&2 || true
    die "timed out waiting for log regex: $pattern"
}

pid=""
cleanup() {
    if [[ -n "${RUN_STAMP:-}" && -e "$RUN_STAMP" ]]; then
        rm -f "$RUN_STAMP"
    fi
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null && [[ "$KEEP_RUNNING" -eq 0 ]]; then
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
    if [[ -n "$XVFB_PID" ]] && kill -0 "$XVFB_PID" 2>/dev/null && [[ "$KEEP_RUNNING" -eq 0 ]]; then
        kill "$XVFB_PID" 2>/dev/null || true
        wait "$XVFB_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

if [[ "$START_XVFB" -eq 1 ]]; then
    command -v Xvfb >/dev/null || die "Xvfb is required for --xvfb"
    command -v xdpyinfo >/dev/null || die "xdpyinfo is required for --xvfb"
    if [[ "$XVFB_DISPLAY" == "auto" ]]; then
        XVFB_DISPLAY="$(choose_xvfb_display)"
    fi
    XVFB_LOG="$(mktemp "${TMPDIR:-/tmp}/capture-face-test-xvfb.XXXXXX.log")"
    DISPLAY="$XVFB_DISPLAY"
    unset XAUTHORITY
    Xvfb "$DISPLAY" -screen 0 "$XVFB_SCREEN" -ac >"$XVFB_LOG" 2>&1 &
    XVFB_PID=$!
    wait_for_x_server
fi

echo "app=$APP"
echo "bin=$BIN"
echo "log=$LOG_FILE"
echo "output=$OUTPUT"
echo "title-hint=$TITLE_HINT"
echo "display=${DISPLAY:-}"
if [[ -n "$XVFB_PID" ]]; then
    echo "xvfb-pid=$XVFB_PID"
    echo "xvfb-log=$XVFB_LOG"
fi
echo "command=${CMD[*]}"

: >"$LOG_FILE"

if [[ "$APP" == "neomacs" ]]; then
    (
        export DISPLAY RUST_LOG="$RUST_LOG_VALUE"
        if [[ -n "${XAUTHORITY:-}" ]]; then
            export XAUTHORITY
        fi
        exec "${CMD[@]}"
    ) >"$LOG_FILE" 2>&1 &
else
    (
        export DISPLAY
        if [[ -n "${XAUTHORITY:-}" ]]; then
            export XAUTHORITY
        fi
        exec "${CMD[@]}"
    ) >"$LOG_FILE" 2>&1 &
fi
pid=$!
echo "pid=$pid"

wid=""
for ((i = 0; i < WAIT_SECONDS; i++)); do
    if ! kill -0 "$pid" 2>/dev/null; then
        tail -n 120 "$LOG_FILE" >&2 || true
        die "$APP exited before opening a visible window"
    fi

    wid="$(xdotool search --onlyvisible --pid "$pid" 2>/dev/null | head -1 || true)"
    if [[ -n "$wid" ]]; then
        break
    fi
    sleep 1
done

[[ -n "$wid" ]] || die "timed out waiting for a visible $APP window"

sleep "$POST_WAIT"

if [[ -n "$WAIT_FOR_LOG_BEFORE_RESIZE" ]]; then
    wait_for_log_pattern "$WAIT_FOR_LOG_BEFORE_RESIZE"
fi

if ! xdotool windowactivate --sync "$wid"; then
    # WM-less X servers like Xvfb do not advertise _NET_ACTIVE_WINDOW.
    # Fall back to direct input focus so scripted capture still works.
    xdotool windowfocus --sync "$wid" || true
fi
xdotool windowraise "$wid" || true
if [[ -n "$WINDOW_SIZE" ]]; then
    xdotool windowsize --sync "$wid" "$WINDOW_SIZE_WIDTH" "$WINDOW_SIZE_HEIGHT"
    sleep "$KEY_DELAY"
    if [[ "$AFTER_RESIZE_WAIT" != "0" ]]; then
        sleep "$AFTER_RESIZE_WAIT"
    fi
fi
sleep "$KEY_DELAY"

for idx in "${!ACTION_KINDS[@]}"; do
    kind="${ACTION_KINDS[$idx]}"
    value="${ACTION_VALUES[$idx]}"
    case "$kind" in
        key)
            xdotool key --window "$wid" --clearmodifiers "$value"
            sleep "$KEY_DELAY"
            ;;
        type)
            xdotool type --window "$wid" --clearmodifiers --delay "$TYPE_DELAY_MS" "$value"
            sleep "$KEY_DELAY"
            ;;
        eval)
            xdotool key --window "$wid" --clearmodifiers alt+shift+semicolon
            sleep "$KEY_DELAY"
            xdotool type --window "$wid" --clearmodifiers --delay "$TYPE_DELAY_MS" "$value"
            sleep "$KEY_DELAY"
            xdotool key --window "$wid" --clearmodifiers Return
            sleep "$KEY_DELAY"
            if [[ "$AFTER_EVAL_WAIT" != "0" ]]; then
                sleep "$AFTER_EVAL_WAIT"
            fi
            ;;
        *)
            die "unknown action kind: $kind"
            ;;
    esac
done

if [[ -n "$WAIT_FOR_FILE" ]]; then
    wait_for_path "$WAIT_FOR_FILE"
fi

if [[ -n "$WAIT_FOR_LOG_PATTERN" ]]; then
    wait_for_log_pattern "$WAIT_FOR_LOG_PATTERN"
fi

if [[ "$AFTER_READY_WAIT" != "0" ]]; then
    sleep "$AFTER_READY_WAIT"
fi

import -window "$wid" "$OUTPUT"

echo "wid=$wid"
echo "window-name=$(xdotool getwindowname "$wid" 2>/dev/null || true)"
echo "geometry:"
xdotool getwindowgeometry "$wid" || true
echo "captured=$OUTPUT"

if [[ "$KEEP_RUNNING" -eq 1 ]]; then
    echo "editor-left-running=1"
fi
