// Minimal static server for local preview of demo/dist.
import { createReadStream, existsSync, statSync } from 'node:fs';
import { createServer } from 'node:http';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, 'dist');
const port = Number(process.env.PORT || 5173);
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.map': 'application/json', '.wasm': 'application/wasm' };

createServer((req, res) => {
  let p = decodeURIComponent((req.url || '/').split('?')[0]);
  if (p === '/') p = '/index.html';
  const file = join(root, p);
  if (!file.startsWith(root) || !existsSync(file) || !statSync(file).isFile()) {
    res.writeHead(404); res.end('not found'); return;
  }
  // No-store: the demo is a single inlined bundle, so a cached main.js silently
  // serves stale code after a rebuild. For a local preview server we always want
  // the freshly-built file.
  res.writeHead(200, {
    'Content-Type': TYPES[extname(file)] || 'application/octet-stream',
    'Cache-Control': 'no-store, must-revalidate',
  });
  createReadStream(file).pipe(res);
}).listen(port, () => console.log(`demo at http://localhost:${port}`));
