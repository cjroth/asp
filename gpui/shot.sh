#!/usr/bin/env bash
exec >/tmp/shot.log 2>&1
BIN="$1"; OUT="${2:-/tmp/shot.png}"
rm -f "$OUT"
export DISPLAY=:99
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
export VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json
export WAYLAND_DISPLAY=
export WGPU_BACKEND=vulkan
export LIBGL_ALWAYS_SOFTWARE=1
pkill -f "Xvfb :99" 2>/dev/null; sleep 0.4
Xvfb :99 -screen 0 1280x900x24 &
XVFB=$!
sleep 1.2
"$BIN" "$OUT" &
APP=$!
for i in $(seq 1 30); do
  [ -f "$OUT" ] && break
  kill -0 $APP 2>/dev/null || break
  sleep 0.5
done
sleep 0.3
kill $APP 2>/dev/null
kill $XVFB 2>/dev/null
echo "--- out: $(stat -c '%s bytes' "$OUT" 2>/dev/null || echo MISSING) ---"
echo SHOT_DONE
