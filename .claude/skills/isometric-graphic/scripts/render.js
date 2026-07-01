// Rasterize an SVG to PNG so you can visually verify an isometric graphic.
// Usage: node render.js input.svg output.png [width] [height]
//
// Uses the pre-installed Chromium via Playwright. Override the browser with
// PW_CHROMIUM=/path/to/chrome if the default location differs.
const { chromium } = require(process.env.PW_MODULE || '/opt/node22/lib/node_modules/playwright');
const fs = require('fs');

(async () => {
  const [svgPath, outPath, w, h] = process.argv.slice(2);
  if (!svgPath || !outPath) {
    console.error('usage: node render.js input.svg output.png [width] [height]');
    process.exit(1);
  }
  const svg = fs.readFileSync(svgPath, 'utf8');
  const exe = process.env.PW_CHROMIUM || '/opt/pw-browsers/chromium';
  const browser = await chromium.launch(fs.existsSync(exe) ? { executablePath: exe } : {});
  const page = await browser.newPage({ deviceScaleFactor: 2 });
  await page.setViewportSize({ width: parseInt(w) || 1120, height: parseInt(h) || 512 });
  await page.setContent(`<!doctype html><html><body style="margin:0">${svg}</body></html>`);
  await page.waitForTimeout(300); // let webfonts settle
  await page.screenshot({ path: outPath });
  await browser.close();
  console.log('wrote', outPath);
})();
