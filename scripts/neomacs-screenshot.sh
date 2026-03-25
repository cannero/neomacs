#!/usr/bin/env bash
# Script to auto-find Emacs window, focus it, and take screenshot
# Usage: ./emacs-screenshot.sh [output.png]
# Expects exactly one Emacs window to be present

set -e

OUTPUT="${1:-/tmp/emacs-screenshot.png}"

# Find emacs process - search for common patterns
EMACS_PIDS=""
for pattern in "./src/emacs" "emacs -Q" "/emacs$"; do
    PIDS=$(pgrep -f "$pattern" 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        EMACS_PIDS="$PIDS"
        break
    fi
done

# Deduplicate PIDs
EMACS_PIDS=$(echo "$EMACS_PIDS" | sort -u | tr '\n' ' ' | xargs)
PID_COUNT=$(echo "$EMACS_PIDS" | wc -w)

if [ "$PID_COUNT" -eq 0 ]; then
    echo "ERROR: No Emacs process found"
    exit 1
fi

echo "Found Emacs PID(s): $EMACS_PIDS ($PID_COUNT total)"

# Collect all windows from all Emacs PIDs
ALL_WIDS=""
for PID in $EMACS_PIDS; do
    WIDS=$(xdotool search --pid "$PID" 2>/dev/null || true)
    ALL_WIDS="$ALL_WIDS $WIDS"
done

# Also search by window name as fallback (GTK4 windows may not link to PID immediately)
NAME_WIDS=$(xdotool search --name "^Emacs$" 2>/dev/null || true)
ALL_WIDS="$ALL_WIDS $NAME_WIDS"
ALL_WIDS=$(echo "$ALL_WIDS" | xargs | tr ' ' '\n' | sort -u | tr '\n' ' ')

if [ -z "$ALL_WIDS" ]; then
    echo "ERROR: No window found for Emacs processes"
    exit 1
fi

echo "Found candidate windows: $ALL_WIDS"

# Filter to windows that have "Emacs" in title and are sizable
VIEWABLE_WIDS=""
for WID in $ALL_WIDS; do
    WNAME=$(xdotool getwindowname "$WID" 2>/dev/null || echo "")
    if [[ "$WNAME" == *"Emacs"* ]] && [[ "$WNAME" != *"Konsole"* ]]; then
        VIEWABLE_WIDS="$VIEWABLE_WIDS $WID"
    fi
done
VIEWABLE_WIDS=$(echo "$VIEWABLE_WIDS" | xargs)

WID_COUNT=$(echo "$VIEWABLE_WIDS" | wc -w)
echo "Found $WID_COUNT viewable Emacs window(s): $VIEWABLE_WIDS"

if [ "$WID_COUNT" -eq 0 ]; then
    echo "ERROR: No viewable Emacs window found"
    exit 1
fi

if [ "$WID_COUNT" -gt 1 ]; then
    echo "WARNING: Multiple viewable windows found, using first one"
fi

# Get the first viewable window
WID=$(echo "$VIEWABLE_WIDS" | awk '{print $1}')
WINNAME=$(xdotool getwindowname "$WID" 2>/dev/null || echo "unknown")
echo "Using window $WID ($WINNAME)"

# Focus the window
echo "Focusing window..."
xdotool windowactivate --sync "$WID" 2>/dev/null || {
    echo "WARNING: windowactivate failed, trying wmctrl..."
    wmctrl -i -a "$WID" 2>/dev/null || true
}
sleep 0.3

# Raise it to front
xdotool windowraise "$WID" 2>/dev/null || true
sleep 0.3

# Take screenshot
echo "Taking screenshot..."
if import -window "$WID" "$OUTPUT" 2>/dev/null; then
    SIZE=$(stat -c%s "$OUTPUT")
    if [ "$SIZE" -gt 1000 ]; then
        echo "SUCCESS: Screenshot saved to $OUTPUT (${SIZE} bytes)"
        exit 0
    fi
fi

# Fallback: capture using the root window cropped to emacs geometry
echo "Trying fallback: capture via root window..."
GEOMETRY=$(xdotool getwindowgeometry "$WID" 2>/dev/null | grep Geometry | awk '{print $2}')
POSITION=$(xdotool getwindowgeometry "$WID" 2>/dev/null | grep Position | awk '{print $2}')
if [ -n "$GEOMETRY" ] && [ -n "$POSITION" ]; then
    import -window root -crop "${GEOMETRY}+${POSITION//,/+}" "$OUTPUT" 2>/dev/null || import -window root "$OUTPUT"
else
    import -window root "$OUTPUT"
fi
SIZE=$(stat -c%s "$OUTPUT")
echo "Fallback screenshot saved to $OUTPUT (${SIZE} bytes)"
