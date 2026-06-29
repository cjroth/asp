#!/usr/bin/env bash
# Source this before building/running a gpui app headlessly on Linux:
#     source gpui-env.sh
# Sets up software Vulkan (lavapipe) so offscreen render + screenshots work with
# no GPU, and exposes a couple of helpers. macOS needs none of this.

# Force the software Vulkan ICD (lavapipe) — no GPU required.
if [ -f /usr/share/vulkan/icd.d/lvp_icd.json ]; then
  export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
  export VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json
fi
export GALLIUM_DRIVER=llvmpipe
export LIBGL_ALWAYS_SOFTWARE=1

# Capture a view to PNG and report whether it's non-blank.
# usage: gpui_shot ./target/debug/myapp out.png editor
gpui_shot() {
  local bin="$1" out="$2" view="${3:-default}"
  "$bin" --shot "$out" "$view" >/dev/null 2>&1
  if command -v convert >/dev/null; then
    echo "$out: $(convert "$out" -format 'colors=%k mean=%[fx:mean]' info: 2>/dev/null)"
  fi
}

# Kill the app SAFELY (never pkill -f the path — it matches this shell and kills it).
# usage: gpui_kill myapp
gpui_kill() { pkill -x "$1" 2>/dev/null; true; }

echo "gpui-env: VK_ICD=${VK_ICD_FILENAMES:-<none>}  (build with: cargo build -j4)"
# Perf-harness tip: add a `--perf` mode that times the hot paths at real scale
# (markdown/tree parse over N iters; N offscreen capture_screenshot frames) and
# prints ms/iter + ms/frame — same HeadlessAppContext setup as --shot, in a loop.
