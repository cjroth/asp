#!/usr/bin/env bash
# Run the scripted screenshot driver headless; it writes PNGs into $1 (outdir).
exec >/tmp/shot.log 2>&1
OUTDIR="${1:-/tmp/aspshots}"
BIN="/home/chris/asp/gpui/target/debug/shoot"
rm -rf "$OUTDIR"; mkdir -p "$OUTDIR"
export DISPLAY=:99
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
export VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json
export WAYLAND_DISPLAY=
export WGPU_BACKEND=vulkan
export LIBGL_ALWAYS_SOFTWARE=1
export ASP_NO_RELAY=1
pkill -f "Xvfb :99" 2>/dev/null; sleep 0.4
Xvfb :99 -screen 0 1280x900x24 &
XVFB=$!
sleep 1.2
"$BIN" "$OUTDIR" &
APP=$!
for i in $(seq 1 40); do
  kill -0 $APP 2>/dev/null || break
  sleep 0.5
done
kill $APP 2>/dev/null
kill $XVFB 2>/dev/null
echo "--- shots ---"; ls -la "$OUTDIR"
echo DRIVER_DONE
