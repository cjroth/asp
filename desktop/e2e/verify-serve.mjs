// Static server for the built web app (REAL wasm engine, no mock backend), so the
// verification drive exercises the real branching/tag/PITR logic in a real browser.
import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, resolve } from 'node:path';

const DIST = resolve(process.argv[2] || join(process.cwd(), 'dist'));
const PORT = Number(process.env.PORT || 5601);
const MIME = {
  '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css', '.html': 'text/html',
  '.woff2': 'font/woff2', '.woff': 'font/woff', '.svg': 'image/svg+xml', '.json': 'application/json',
  '.wasm': 'application/wasm',
};

http
  .createServer(async (req, res) => {
    try {
      const url = (req.url || '/').split('?')[0];
      const path = url === '/' ? '/index.html' : url;
      const f = join(DIST, path);
      const data = await readFile(f);
      res.setHeader('Content-Type', MIME[extname(f)] || 'application/octet-stream');
      // OPFS + wasm want a secure-ish context; localhost qualifies. Add COOP/COEP
      // so any future SharedArrayBuffer use keeps working.
      res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
      res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
      res.end(data);
    } catch {
      // SPA fallback → index.html
      try {
        const html = await readFile(join(DIST, 'index.html'));
        res.setHeader('Content-Type', 'text/html');
        res.end(html);
      } catch {
        res.statusCode = 404;
        res.end('not found');
      }
    }
  })
  .listen(PORT, () => console.log('serving ' + DIST + ' on http://127.0.0.1:' + PORT));
