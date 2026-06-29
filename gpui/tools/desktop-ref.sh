#!/usr/bin/env bash
# Capture a reference screenshot of the REAL desktop app (its web build) and
# pixel-diff it against the gpui `--shot`. Drives the chromium that Playwright
# downloads (no apt/snap browser needed), so it runs in this sandbox too.
#
#   tools/desktop-ref.sh            # capture desktop connect + diff vs gpui connect-web
#
# Prereqs (one-time): `cd ../../desktop && bun run build:web` to produce dist/,
# and `bunx playwright install chromium` to fetch the browser.
set -uo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"            # the gpui crate dir
DESKTOP="$HERE/../desktop"
PORT=8099
OUT="$HERE/tools/shots/desktop-connect.png"

CHROME=$(ls ~/.cache/ms-playwright/chromium-*/chrome-linux/chrome 2>/dev/null | sort | tail -1)
[ -z "$CHROME" ] && { echo "no Playwright chromium — run: bunx playwright install chromium"; exit 1; }

# Serve the built web app.
( python3 -m http.server "$PORT" --directory "$DESKTOP/dist" >/tmp/asp-dist-serve.log 2>&1 & )
SERVE_PID=$!
sleep 1.5

# Drive chromium via playwright-core with an explicit executablePath.
CAP=$(mktemp -d)
( cd "$CAP" && bun add playwright-core >/dev/null 2>&1 )
cat > "$CAP/cap.mjs" <<EOF
import { chromium } from 'playwright-core';
const b = await chromium.launch({ headless: true, executablePath: process.env.CHROME, args:['--no-sandbox','--disable-gpu'] });
const p = await b.newPage({ viewport: { width: 1100, height: 740 }, deviceScaleFactor: 2 });
await p.goto('http://localhost:$PORT/', { waitUntil: 'networkidle' });
await p.waitForTimeout(4000);
await p.screenshot({ path: process.env.OUT });
await b.close();
EOF
CHROME="$CHROME" OUT="$OUT" node "$CAP/cap.mjs" && echo "desktop ref → $OUT"

# Build the matching gpui state + diff.
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json "$HERE/target/debug/asp-gpui" --shot "$HERE/tools/shots/connect-web.png" connect-web >/dev/null 2>&1
bash "$HERE/tools/diff.sh" "$OUT" "$HERE/tools/shots/connect-web.png" "$HERE/tools/shots/diff-connect.png"
