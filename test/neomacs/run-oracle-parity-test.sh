#!/usr/bin/env bash
# Compare official Emacs vs neomacs rendering for key parity scenarios.
#
# Scenarios:
#   1) view-hello-file column alignment
#   2) CJK cursor placement on a好好b (cursor on first/second 好)
#   3) Noto/Hack bold vs extra-bold rendering lines
#
# Optional env:
#   OFFICIAL_EMACS=/path/to/emacs
#   NEOMACS_BIN=./src/emacs
#   ORACLE_OUT_DIR=/tmp/neomacs-oracle-parity
#   ORACLE_MAX_AE=<int>   # fail if AE diff exceeds threshold

set -euo pipefail

cd "$(dirname "$0")/../.."

DISPLAY_ENV="${DISPLAY:-:0}"
ORACLE_ELISP="test/neomacs/neomacs-oracle-parity.el"
OFFICIAL_EMACS="${OFFICIAL_EMACS:-/nix/store/hql3zwz5b4ywd2qwx8jssp4dyb7nx4cb-emacs-30.2/bin/emacs}"
NEOMACS_BIN="${NEOMACS_BIN:-./src/emacs}"
OUT_DIR="${ORACLE_OUT_DIR:-/tmp/neomacs-oracle-parity}"

if ! command -v xdotool >/dev/null 2>&1; then
    echo "SKIP: xdotool not found."
    exit 0
fi
if ! command -v import >/dev/null 2>&1; then
    echo "SKIP: ImageMagick import not found."
    exit 0
fi
if ! command -v "$OFFICIAL_EMACS" >/dev/null 2>&1; then
    echo "SKIP: official emacs not found at $OFFICIAL_EMACS"
    exit 0
fi
if ! command -v "$NEOMACS_BIN" >/dev/null 2>&1; then
    echo "SKIP: neomacs binary not found at $NEOMACS_BIN"
    exit 0
fi

mkdir -p "$OUT_DIR"

capture_case() {
    local bin="$1"
    local label="$2"
    local scenario="$3"
    local title="neomacs-oracle-${scenario}-${label}"
    local png="${OUT_DIR}/${scenario}-${label}.png"
    local log="${OUT_DIR}/${scenario}-${label}.log"

    env DISPLAY="$DISPLAY_ENV" \
        NEOMACS_ORACLE_SCENARIO="$scenario" \
        NEOMACS_ORACLE_LABEL="$label" \
        "$bin" -Q -l "$ORACLE_ELISP" >"$log" 2>&1 &
    local pid=$!

    local win_id=""
    for _ in $(seq 1 60); do
        win_id=$(DISPLAY="$DISPLAY_ENV" xdotool search --name "$title" 2>/dev/null | tail -1 || true)
        if [ -n "$win_id" ]; then
            break
        fi
        sleep 0.2
    done

    if [ -z "$win_id" ]; then
        echo "ERROR: could not find window for ${title} (log: ${log})"
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
        return 1
    fi

    DISPLAY="$DISPLAY_ENV" xdotool windowactivate --sync "$win_id" >/dev/null 2>&1 || true
    if command -v compare >/dev/null 2>&1; then
        local prev_png curr_png attempts stable status ae image_geom expected_geom
        prev_png=$(mktemp "${OUT_DIR}/.${scenario}-${label}.prev.XXXXXX.png")
        curr_png=$(mktemp "${OUT_DIR}/.${scenario}-${label}.curr.XXXXXX.png")
        stable=0

        for attempts in $(seq 1 20); do
            sleep 0.2
            DISPLAY="$DISPLAY_ENV" import -window "$win_id" "$curr_png" 2>/dev/null || true
            if [ ! -s "$curr_png" ]; then
                continue
            fi
            eval "$(DISPLAY="$DISPLAY_ENV" xdotool getwindowgeometry --shell "$win_id")"
            expected_geom="${WIDTH}x${HEIGHT}"
            image_geom=$(identify -format '%wx%h' "$curr_png" 2>/dev/null || true)
            if [ "$image_geom" != "$expected_geom" ]; then
                stable=0
                continue
            fi

            if [ -s "$prev_png" ]; then
                set +e
                ae=$(compare -metric AE "$prev_png" "$curr_png" null: 2>&1 >/dev/null)
                status=$?
                set -e
                if [ "$status" -lt 2 ] && [ "${ae}" = "0" ] && [ "$attempts" -ge 4 ]; then
                    stable=$((stable + 1))
                    if [ "$stable" -ge 2 ]; then
                        break
                    fi
                else
                    stable=0
                fi
            fi

            cp -f "$curr_png" "$prev_png"
        done

        mv -f "$curr_png" "$png"
        rm -f "$prev_png"
    else
        sleep 1.5
        DISPLAY="$DISPLAY_ENV" import -window "$win_id" "$png" 2>/dev/null
    fi

    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true

    echo "Captured ${png}"
}

compare_case() {
    local scenario="$1"
    local off_png="${OUT_DIR}/${scenario}-official.png"
    local neo_png="${OUT_DIR}/${scenario}-neomacs.png"
    local diff_png="${OUT_DIR}/${scenario}-diff.png"

    if ! command -v compare >/dev/null 2>&1; then
        echo "compare unavailable; skipped metric for ${scenario}"
        return 0
    fi

    local ae
    local rmse
    local status

    set +e
    ae=$(compare -metric AE "$off_png" "$neo_png" "$diff_png" 2>&1 >/dev/null)
    status=$?
    set -e
    if [ "$status" -ge 2 ]; then
        echo "ERROR: compare AE failed for ${scenario}"
        return 1
    fi

    set +e
    rmse=$(compare -metric RMSE "$off_png" "$neo_png" null: 2>&1 >/dev/null)
    status=$?
    set -e
    if [ "$status" -ge 2 ]; then
        echo "ERROR: compare RMSE failed for ${scenario}"
        return 1
    fi

    echo "scenario=${scenario} AE=${ae} RMSE=${rmse}"

    if [ -n "${ORACLE_MAX_AE:-}" ] && [[ "$ae" =~ ^[0-9]+$ ]]; then
        if [ "$ae" -gt "$ORACLE_MAX_AE" ]; then
            echo "FAIL: ${scenario} AE ${ae} > ORACLE_MAX_AE ${ORACLE_MAX_AE}"
            return 1
        fi
    fi

    return 0
}

SCENARIOS=(hello cjk1 cjk2 weights)
FAILED=0

for s in "${SCENARIOS[@]}"; do
    capture_case "$OFFICIAL_EMACS" "official" "$s" || FAILED=1
    capture_case "$NEOMACS_BIN" "neomacs" "$s" || FAILED=1
done

for s in "${SCENARIOS[@]}"; do
    compare_case "$s" || FAILED=1
done

echo "Oracle artifacts: ${OUT_DIR}"
if [ "$FAILED" -ne 0 ]; then
    exit 1
fi

exit 0
