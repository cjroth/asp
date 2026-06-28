#!/usr/bin/env bash
# Headless GUI harness for the asp-gpui app.
# Provides: asp_env, asp_launch, asp_shot, asp_kill — for human-like eval of the
# native gpui app under Xvfb + lavapipe (software Vulkan) with no real display/GPU.
set -uo pipefail

ASP_GPUI_DIR="${ASP_GPUI_DIR:-/home/chris/asp/.claude/worktrees/gpui-design-match/gpui}"
ASP_DISPLAY_NUM="${ASP_DISPLAY_NUM:-:77}"
ASP_W="${ASP_W:-1280}"
ASP_H="${ASP_H:-900}"
ASP_BIN="${ASP_BIN:-$ASP_GPUI_DIR/target/debug/asp-gpui}"
ASP_SHOTDIR="${ASP_SHOTDIR:-$ASP_GPUI_DIR/tools/shots}"

asp_env() {
  export DISPLAY="$ASP_DISPLAY_NUM"
  export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
  export VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json
  export GALLIUM_DRIVER=llvmpipe
  export LIBGL_ALWAYS_SOFTWARE=1
  unset WAYLAND_DISPLAY
  mkdir -p "$ASP_SHOTDIR"
  if ! xdpyinfo -display "$ASP_DISPLAY_NUM" >/dev/null 2>&1; then
    rm -f "/tmp/.X${ASP_DISPLAY_NUM#:}-lock" 2>/dev/null
    Xvfb "$ASP_DISPLAY_NUM" -screen 0 "${ASP_W}x${ASP_H}x24" -ac -nolisten tcp \
      >/tmp/asp-xvfb${ASP_DISPLAY_NUM#:}.log 2>&1 &
    for _ in $(seq 1 20); do xdpyinfo -display "$ASP_DISPLAY_NUM" >/dev/null 2>&1 && break; sleep 0.2; done
  fi
}

# asp_launch [extra args...] -> writes PID to $ASP_SHOTDIR/app.pid, logs to app.log
asp_launch() {
  asp_env
  asp_kill >/dev/null 2>&1
  RUST_BACKTRACE=1 "$ASP_BIN" "$@" >"$ASP_SHOTDIR/app.log" 2>&1 &
  echo $! >"$ASP_SHOTDIR/app.pid"
  # wait for the window to appear and first frame to render
  sleep "${ASP_BOOT_WAIT:-3}"
}

# asp_shot <name> -> $ASP_SHOTDIR/<name>.png
asp_shot() {
  local name="${1:-shot}"
  import -display "$ASP_DISPLAY_NUM" -window root "$ASP_SHOTDIR/$name.png" 2>/dev/null \
    || xwd -display "$ASP_DISPLAY_NUM" -root -silent | convert xwd:- "$ASP_SHOTDIR/$name.png"
  echo "$ASP_SHOTDIR/$name.png"
}

asp_kill() {
  [ -f "$ASP_SHOTDIR/app.pid" ] && kill "$(cat "$ASP_SHOTDIR/app.pid")" 2>/dev/null
  pkill -f 'target/debug/asp-gpui' 2>/dev/null
  true
}

"$@"
