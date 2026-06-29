#!/usr/bin/env bash
# Pixel-diff two screenshots. Reports differing-pixel count + % and writes a
# diff image (changed regions in red). Resizes B to A's dimensions first, so
# captures at different scale factors still compare.
#
#   diff.sh A.png B.png [out-diff.png]
set -euo pipefail

A="${1:?usage: diff.sh A.png B.png [out.png]}"
B="${2:?usage: diff.sh A.png B.png [out.png]}"
OUT="${3:-diff.png}"

# magick (IM7) or convert/compare (IM6)
if command -v magick >/dev/null; then M="magick"; CMP="magick compare"; else M="convert"; CMP="compare"; fi

DIMS=$($M identify -format '%wx%h' "$A")
$M "$B" -resize "${DIMS}!" /tmp/_vdiff_b.png

# AE = count of pixels that differ (fuzz ignores anti-aliasing noise).
# compare prints AE to stderr; the first token is the count (may be scientific
# notation like 3.24656e+06 for large diffs — let awk parse it, don't strip it).
RAW=$($CMP -metric AE -fuzz 3% "$A" /tmp/_vdiff_b.png "$OUT" 2>&1 || true)
AE=$(printf '%s\n' "$RAW" | awk '{print $1; exit}')
TOTAL=$($M identify -format '%[fx:w*h]' "$A")
PCT=$(awk -v ae="${AE:-0}" -v tot="$TOTAL" 'BEGIN { printf "%.3f", (ae/tot)*100 }')

echo "A:        $A ($DIMS)"
echo "B:        $B (resized to $DIMS)"
echo "diff img: $OUT   (red = differing pixels — open it)"
echo "AE:       ${AE:-0} of $TOTAL pixels"
echo "diff:     ${PCT}%"
