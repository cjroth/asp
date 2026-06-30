// Dev automation bridge server — the host side of src/lib/devbridge.ts.
//
// Why: macOS ships no WebDriver for embedded WKWebView, so there's no built-in
// way to drive/observe a running Tauri (desktop) webview the way CDP drives
// Chrome. Since the same Vite app runs in the Tauri WKWebView *and* a browser
// tab, devbridge.ts dials this server from whichever surface loaded it and
// registers as 'desktop' or 'web'. This server then forwards JavaScript to a
// surface and returns the result — full JS/DOM access to either side, no GUI
// clicking, no native driver. DEV-only: devbridge.ts is tree-shaken out of
// production builds.
//
// Run (from desktop/, with `bun run dev` already up):
//   bun e2e/dev-bridge-server.mjs
//
// Drive a surface (the code may `return` and `await`; `api` is in scope):
//   curl -s 'localhost:17999/eval?surface=desktop' \
//     --data "return await api.listVaults()"
//   curl -s 'localhost:17999/eval?surface=web' \
//     --data "return await api.readFile((await api.listVaults())[0].id, 'README.md')"
//   curl -s localhost:17999/clients        # which surfaces are connected
const clients = new Map(); // surface -> ws
const pending = new Map(); // id -> resolve
let counter = 1;

const server = Bun.serve({
  port: 17999,
  idleTimeout: 60,
  async fetch(req, server) {
    const url = new URL(req.url);
    if (url.pathname === '/ws') {
      if (server.upgrade(req)) return;
      return new Response('upgrade failed', { status: 500 });
    }
    if (url.pathname === '/clients') {
      return Response.json({ surfaces: [...clients.keys()] });
    }
    if (url.pathname === '/eval' && req.method === 'POST') {
      const surface = url.searchParams.get('surface') || 'web';
      const code = await req.text();
      const ws = clients.get(surface);
      if (!ws) return Response.json({ ok: false, error: `no client for surface '${surface}' (have: ${[...clients.keys()].join(',') || 'none'})` }, { status: 404 });
      const id = counter++;
      const result = await new Promise((resolve) => {
        pending.set(id, resolve);
        setTimeout(() => { if (pending.delete(id)) resolve({ ok: false, error: 'timeout' }); }, 25000);
        ws.send(JSON.stringify({ type: 'eval', id, code }));
      });
      return Response.json(result);
    }
    return new Response('bridge ok');
  },
  websocket: {
    message(ws, message) {
      let msg;
      try { msg = JSON.parse(String(message)); } catch { return; }
      if (msg.type === 'hello') {
        clients.set(msg.surface, ws);
        ws.data = { surface: msg.surface };
        console.log(`[bridge] connected: ${msg.surface}`);
      } else if (msg.type === 'result') {
        const resolve = pending.get(msg.id);
        if (resolve) { pending.delete(msg.id); resolve({ ok: msg.ok, value: msg.value, error: msg.error }); }
      }
    },
    close(ws) {
      const s = ws.data?.surface;
      if (s && clients.get(s) === ws) { clients.delete(s); console.log(`[bridge] disconnected: ${s}`); }
    },
  },
});
console.log(`[bridge] listening on http://localhost:${server.port}`);
