# Source this to get a headless software-Vulkan GUI environment for gpui.
# Starts Xvfb on $ASP_DISPLAY (default :99) if not already running.
export ASP_DISPLAY="${ASP_DISPLAY:-:99}"
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
export VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json
export GALLIUM_DRIVER=llvmpipe
export LIBGL_ALWAYS_SOFTWARE=1
# gpui: prefer X11 (Xvfb), never Wayland in headless.
unset WAYLAND_DISPLAY
export ZED_WINDOW_BACKEND=x11 2>/dev/null || true

asp_start_xvfb() {
  if ! xdpyinfo -display "$ASP_DISPLAY" >/dev/null 2>&1; then
    Xvfb "$ASP_DISPLAY" -screen 0 1280x900x24 -ac +extension GLX +render -noreset >/tmp/asp-xvfb.log 2>&1 &
    sleep 1.5
  fi
  export DISPLAY="$ASP_DISPLAY"
}
