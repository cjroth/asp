// Static server for the built frontend, injecting the mock Tauri backend before
// the app bundle so the real UI runs in a real browser without a Tauri shell.
import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join } from 'node:path';

const ROOT = '/home/chris/asp/desktop';
const DIST = join(ROOT, 'dist');
const MOCK = join(ROOT, 'e2e', 'mock-backend.js');
const PORT = Number(process.env.PORT || 5599);
const MIME = {
  '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css', '.html': 'text/html',
  '.woff2': 'font/woff2', '.woff': 'font/woff', '.svg': 'image/svg+xml', '.json': 'application/json',
};

http
  .createServer(async (req, res) => {
    try {
      const url = (req.url || '/').split('?')[0];
      if (url === '/mock-backend.js') {
        res.setHeader('Content-Type', 'text/javascript');
        return res.end(await readFile(MOCK));
      }
      if (url === '/' || url === '/index.html') {
        let html = await readFile(join(DIST, 'index.html'), 'utf8');
        html = html.replace('<head>', '<head>\n    <script src="/mock-backend.js"></script>');
        res.setHeader('Content-Type', 'text/html');
        return res.end(html);
      }
      const f = join(DIST, url);
      const data = await readFile(f);
      res.setHeader('Content-Type', MIME[extname(f)] || 'application/octet-stream');
      res.end(data);
    } catch {
      res.statusCode = 404;
      res.end('not found');
    }
  })
  .listen(PORT, () => console.log('serving dist on http://127.0.0.1:' + PORT));
