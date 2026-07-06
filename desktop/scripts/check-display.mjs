#!/usr/bin/env node
// Preflight for `bun dev` (tauri dev) on Linux: the native window is GTK, and
// without a display server GTK panics deep inside tao ("Failed to initialize
// gtk backend!") — a confusing crash for what is really "this VM is headless".
// Catch it here with an actionable message instead. macOS/Windows never need
// an X/Wayland display, so they pass straight through.
if (process.platform === 'linux' && !process.env.DISPLAY && !process.env.WAYLAND_DISPLAY) {
  console.error(`
This machine has no display server (DISPLAY/WAYLAND_DISPLAY are unset), so the
native Tauri window cannot open here (GTK would panic).

Pick the path that fits what you're doing:

  • Develop the UI from a browser (recommended in a headless VM — OrbStack
    forwards localhost to the Mac host automatically):

        bun run dev:web        # then open http://localhost:1420 on the host

  • Run the NATIVE app headless (invisible window, for e2e/driving tools):

        xvfb-run -a bun dev

  • On a machine with a real desktop session, plain \`bun dev\` works as-is.
`);
  process.exit(1);
}
