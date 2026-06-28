#!/usr/bin/env bash
exec >/tmp/dbg.log 2>&1
export DISPLAY=:99
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
export VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json
export WAYLAND_DISPLAY=
export WGPU_BACKEND=vulkan
pkill -f "Xvfb :99"; sleep 0.4
Xvfb :99 -screen 0 1100x760x24 -nolisten tcp &
XVFB=$!
sleep 1.2
RUST_BACKTRACE=1 /home/chris/asp/gpui/target/debug/smoke >/tmp/app.log 2>&1 &
APP=$!
sleep 6
# find the GPUI window id (the child that isn't 1x1)
WID=$(xwininfo -root -tree | grep -oE '0x[0-9a-f]+ \(has no name\): \(\)  [0-9]+x[0-9]+' | grep -vE ' 1x1' | head -1 | grep -oE '^0x[0-9a-f]+')
echo "GPUI window id: $WID"
import -window "$WID" /tmp/win.png 2>/tmp/import.log
echo "import rc=$? ; win.png bytes: $(stat -c %s /tmp/win.png 2>/dev/null)"
cat /tmp/import.log
# also try xwd->png for the window
xwd -id "$WID" -out /tmp/win.xwd 2>/tmp/xwd.log && convert /tmp/win.xwd /tmp/win2.png 2>/dev/null && echo "win2.png bytes: $(stat -c %s /tmp/win2.png)"
kill $APP 2>/dev/null; kill $XVFB 2>/dev/null
echo done
