#!/usr/bin/env bash
# Pixel-diff two screenshots and report the difference. Use it to compare a
# gpui `--shot` capture against a reference shot of the desktop app for the same
# screen/state (capture the desktop refs on a machine with a browser/native app).
#
#   tools/diff.sh <a.png> <b.png> [out-diff.png]
#
# Prints the absolute-error pixel count (AE) and the % of pixels that differ;
# writes a visual diff highlighting the changed regions in red.
set -euo pipefail

A="${1:?usage: diff.sh A.png B.png [out.png]}"
B="${2:?usage: diff.sh A.png B.png [out.png]}"
OUT="${3:-diff.png}"

# Normalize sizes (reference and gpui shot may differ in scale): resize B to A.
DIMS=$(magick identify -format '%wx%h' "$A")
magick "$B" -resize "$DIMS!" /tmp/asp-diff-b.png

# AE = number of pixels that differ (with a small fuzz to ignore AA noise).
AE=$(magick compare -metric AE -fuzz 3% "$A" /tmp/asp-diff-b.png "$OUT" 2>&1 || true)
TOTAL=$(magick identify -format '%[fx:w*h]' "$A")
PCT=$(awk -v ae="$AE" -v tot="$TOTAL" 'BEGIN { printf "%.3f", (ae/tot)*100 }')

echo "A:        $A ($DIMS)"
echo "B:        $B (resized to $DIMS)"
echo "diff img: $OUT"
echo "AE:       $AE differing pixels of $TOTAL"
echo "diff:     ${PCT}%"
