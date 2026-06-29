---
name: gpui-visual-diff
description: Pixel-diff two screenshots (ImageMagick — % differing pixels + a highlighted diff image) and/or capture a REFERENCE app (a web/Electron/React original) via Playwright's own bundled Chromium — no system browser, apt, or snap needed. Use to verify a gpui (or any native) UI is pixel-perfect against an existing design/app, for visual-regression checks, or to compare two render outputs. Pairs with gpui-headless-screenshot (which produces the native PNGs to compare).
---

# Visual diff & reference capture

Two reusable pieces for "is my native UI pixel-perfect vs the original?":

1. **`scripts/diff.sh A.png B.png [out.png]`** — reports the number/% of differing
   pixels (ImageMagick `compare -metric AE`, with a small fuzz to ignore AA noise)
   and writes a diff image highlighting changed regions in red. Resizes B to A so
   different scale factors (e.g. 1x vs 2x) still compare. Self-diff = 0%; a good
   independent reimplementation typically lands ~1–3% (residual is font
   rasterization + any genuinely-different text), with the diff image showing the
   residual is fonts/AA and not layout. **Read the diff image** — it pinpoints real
   bugs (it caught an inverted theme icon and a wrong indicator color in practice).

2. **`scripts/capture-web.sh <url> <out.png> [W H]`** — screenshots a running web
   app via the Chromium that Playwright downloads to `~/.cache/ms-playwright`
   (works in sandboxes where no browser is apt/snap-installable). It drives the
   browser through `playwright-core` with an explicit `executablePath`, so it does
   not care which Playwright version the browser revision matches.

## Typical flow (gpui port vs its web/React original)

```bash
# one-time: fetch a browser (downloads its own; ~170MB)
bunx playwright install chromium     # or: npx playwright install chromium

# 1. serve the original app's build and capture it at the same size as your shot
python3 -m http.server 8099 --directory /path/to/original/dist &
bash <skill>/scripts/capture-web.sh http://localhost:8099/ ref.png 1100 740

# 2. capture the native gpui view (see the gpui-headless-screenshot skill)
./target/debug/myapp --shot mine.png connect

# 3. diff
bash <skill>/scripts/diff.sh ref.png mine.png diff.png
#   AE: 35009 differing pixels of 3.256e+06
#   diff: 1.075%
# then Read diff.png to see WHERE it differs (red = changed)
```

## Notes
- Match window size + state between the two captures (empty vs populated, light vs
  dark, web vs desktop platform copy) — otherwise the diff is dominated by content
  differences, not rendering. Make a fixture for the exact state you're comparing.
- Expect a nonzero floor from different text rasterizers (browser vs gpui's
  cosmic-text) and a few px of layout from font-metric-driven block heights — judge
  by the diff IMAGE (is the red just glyph edges?), not the % alone.
- For native-vs-native regression (same renderer), self-consistency is exact, so
  treat any nonzero diff as a real change.
