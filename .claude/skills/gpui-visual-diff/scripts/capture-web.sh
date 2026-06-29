#!/usr/bin/env bash
# Screenshot a running web app using the Chromium that Playwright downloads
# (no apt/snap/system browser needed — works in sandboxes). Drives the browser
# via playwright-core with an explicit executablePath, so it does not require the
# Playwright package version to match the installed browser revision.
#
#   capture-web.sh <url> <out.png> [width] [height] [device-scale]
#
# One-time prereq:  bunx playwright install chromium   (or: npx playwright install chromium)
set -uo pipefail

URL="${1:?usage: capture-web.sh URL OUT.png [W H scale]}"
OUT="${2:?usage: capture-web.sh URL OUT.png [W H scale]}"
W="${3:-1100}"; H="${4:-740}"; SCALE="${5:-2}"

CHROME=$(ls ~/.cache/ms-playwright/chromium-*/chrome-linux/chrome 2>/dev/null | sort | tail -1)
[ -z "$CHROME" ] && CHROME=$(ls ~/.cache/ms-playwright/chromium-*/chrome-mac*/Chromium.app/Contents/MacOS/Chromium 2>/dev/null | sort | tail -1)
[ -z "$CHROME" ] && { echo "no Playwright Chromium — run: bunx playwright install chromium"; exit 1; }

RUN=$(command -v bun >/dev/null && echo bun || echo node)
ADD=$(command -v bun >/dev/null && echo "bun add" || echo "npm install --no-save")

WORK=$(mktemp -d)
( cd "$WORK" && $ADD playwright-core >/dev/null 2>&1 )
cat > "$WORK/cap.mjs" <<EOF
import { chromium } from 'playwright-core';
const b = await chromium.launch({ headless: true, executablePath: process.env.CHROME, args: ['--no-sandbox','--disable-gpu'] });
const p = await b.newPage({ viewport: { width: $W, height: $H }, deviceScaleFactor: $SCALE });
await p.goto(process.env.URL, { waitUntil: 'networkidle' });
await p.waitForTimeout(4000); // let wasm/JS settle
await p.screenshot({ path: process.env.OUT });
await b.close();
console.log('captured ' + process.env.OUT);
EOF
CHROME="$CHROME" URL="$URL" OUT="$OUT" "$RUN" "$WORK/cap.mjs"
rm -rf "$WORK"
