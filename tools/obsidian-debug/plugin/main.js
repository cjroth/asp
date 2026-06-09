'use strict';
// ASP Debug Bridge — a DEV-ONLY companion Obsidian plugin.
//
// Why this exists: asp is developed inside an OrbStack Linux VM, but Obsidian
// (and the `agent-sync` plugin under test) runs on the Mac host. The vault
// itself is reachable from the VM over the virtiofs mount, so file create/
// delete/edit and swapping in a freshly-built main.js are plain filesystem ops.
// What the mount CANNOT do is talk to the *running* Obsidian process — reload a
// plugin, read the renderer console, or eval JS against the live `app`. This
// plugin closes exactly that gap: it runs a tiny HTTP server bound to the Mac's
// loopback, and the VM dials OUT to it via OrbStack's `host.docker.internal`
// (no socat relay needed — outbound from the VM just works).
//
// Everything reduces to `POST /eval`: reload/reset/state are just JS snippets
// the MCP sends. The one thing eval can't do retroactively is see console
// output that already happened, so we install console + error hooks at load and
// keep a ring buffer, exposed at `GET /console`.
//
// Written as hand-authored CommonJS (no build step): it only needs `obsidian`
// and Node's `http`, both available in the desktop plugin context.

const obsidian = require('obsidian');
const http = require('http');

const DEFAULT_PORT = 28900;
const CONSOLE_CAP = 2000;
// A shared secret so a stray localhost process can't drive Obsidian. Not real
// security (loopback + a dev tool) — just a guard. Override with the same value
// on the MCP side via ASP_DEBUG_TOKEN.
const DEFAULT_TOKEN = 'asp-debug-bridge';

module.exports = class AspDebugBridge extends obsidian.Plugin {
  async onload() {
    this.port = Number(process.env.ASP_DEBUG_PORT) || DEFAULT_PORT;
    this.token = process.env.ASP_DEBUG_TOKEN || DEFAULT_TOKEN;
    this.consoleBuf = [];
    this.installConsoleHooks();
    this.startServer();
    this.addCommand({
      id: 'asp-debug-restart-server',
      name: 'Restart debug bridge server',
      callback: () => {
        this.stopServer();
        this.startServer();
      },
    });
    console.log(`[asp-debug] bridge loaded — listening on 127.0.0.1:${this.port}`);
  }

  onunload() {
    this.stopServer();
    this.restoreConsoleHooks();
  }

  // ---- console / error capture -------------------------------------------

  installConsoleHooks() {
    this._orig = {};
    const push = (level, args) => {
      this.consoleBuf.push({ ts: nowStamp(), level, text: args.map(fmt).join(' ') });
      if (this.consoleBuf.length > CONSOLE_CAP) this.consoleBuf.shift();
    };
    for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
      this._orig[level] = console[level];
      console[level] = (...args) => {
        try {
          push(level, args);
        } catch {
          /* never let capture break logging */
        }
        return this._orig[level].apply(console, args);
      };
    }
    this._onError = (e) => push('error', [`window.onerror: ${e.message}`, e.error?.stack || '']);
    this._onRej = (e) =>
      push('error', [`unhandledrejection: ${fmt(e.reason)}`, e.reason?.stack || '']);
    window.addEventListener('error', this._onError);
    window.addEventListener('unhandledrejection', this._onRej);
  }

  restoreConsoleHooks() {
    if (this._orig) for (const k of Object.keys(this._orig)) console[k] = this._orig[k];
    if (this._onError) window.removeEventListener('error', this._onError);
    if (this._onRej) window.removeEventListener('unhandledrejection', this._onRej);
  }

  // ---- HTTP server --------------------------------------------------------

  startServer() {
    this.server = http.createServer((req, res) => {
      this.handle(req, res).catch((e) => sendJson(res, 500, { ok: false, error: String(e) }));
    });
    this.server.on('error', (e) => console.error('[asp-debug] server error:', e));
    this.server.listen(this.port, '127.0.0.1');
  }

  stopServer() {
    try {
      this.server?.close();
    } catch {
      /* ignore */
    }
    this.server = undefined;
  }

  async handle(req, res) {
    const url = new URL(req.url, `http://127.0.0.1:${this.port}`);
    // Auth: header or ?token=. /ping is allowed unauthenticated for discovery.
    const tok = req.headers['x-asp-token'] || url.searchParams.get('token');
    if (url.pathname !== '/ping' && tok !== this.token) {
      return sendJson(res, 401, { ok: false, error: 'bad or missing token' });
    }

    if (url.pathname === '/ping') {
      const asp = this.app.plugins?.plugins?.['agent-sync'];
      return sendJson(res, 200, {
        ok: true,
        obsidianVersion: obsidian.apiVersion,
        vault: this.app.vault.getName(),
        bridgeVersion: this.manifest.version,
        asp: asp
          ? { present: true, version: asp.manifest?.version, syncState: asp.syncState }
          : { present: false },
      });
    }

    if (url.pathname === '/console' && req.method === 'GET') {
      const level = url.searchParams.get('level');
      const limit = Number(url.searchParams.get('limit')) || 0;
      let out = this.consoleBuf;
      if (level) out = out.filter((e) => e.level === level);
      if (limit > 0) out = out.slice(-limit);
      const snapshot = out.slice();
      if (url.searchParams.get('clear') === '1') this.consoleBuf = [];
      return sendJson(res, 200, { ok: true, entries: snapshot });
    }

    if (url.pathname === '/eval' && req.method === 'POST') {
      const body = await readBody(req);
      let code;
      try {
        code = JSON.parse(body || '{}').code;
      } catch {
        return sendJson(res, 400, { ok: false, error: 'body must be JSON {code}' });
      }
      if (typeof code !== 'string') {
        return sendJson(res, 400, { ok: false, error: 'missing "code" string' });
      }
      return this.runEval(code, res);
    }

    return sendJson(res, 404, { ok: false, error: `no route ${req.method} ${url.pathname}` });
  }

  // Eval arbitrary JS against the live app. `app`, `plugin`, `obsidian` are in
  // scope; the snippet is wrapped in an async IIFE so it can `await` and
  // `return`. CSP allows Function-construction in the plugin context (the same
  // mechanism Dataview/Templater use to run user JS).
  async runEval(code, res) {
    try {
      const fn = new Function(
        'app',
        'plugin',
        'obsidian',
        `return (async () => { ${code} })();`,
      );
      const result = await fn(this.app, this, obsidian);
      return sendJson(res, 200, { ok: true, result: safeSerialize(result) });
    } catch (e) {
      return sendJson(res, 200, { ok: false, error: String(e?.message ?? e), stack: e?.stack });
    }
  }
};

// ---- helpers --------------------------------------------------------------

function nowStamp() {
  const d = new Date();
  const p = (n, w = 2) => String(n).padStart(w, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}

function fmt(v) {
  if (typeof v === 'string') return v;
  if (v instanceof Error) return `${v.name}: ${v.message}`;
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

// Make an eval result JSON-safe: strip circular refs, render functions/
// undefined as markers, and cap size so a huge object can't wedge the response.
function safeSerialize(v) {
  const seen = new WeakSet();
  const walk = (x, depth) => {
    if (x === undefined) return '<undefined>';
    if (x === null || typeof x === 'number' || typeof x === 'boolean' || typeof x === 'string')
      return x;
    if (typeof x === 'function') return `<function ${x.name || 'anon'}>`;
    if (typeof x === 'bigint') return `${x}n`;
    if (x instanceof Error) return { name: x.name, message: x.message, stack: x.stack };
    if (depth > 6) return '<max-depth>';
    if (typeof x === 'object') {
      if (seen.has(x)) return '<circular>';
      seen.add(x);
      if (Array.isArray(x)) return x.slice(0, 500).map((e) => walk(e, depth + 1));
      const out = {};
      let n = 0;
      for (const k of Object.keys(x)) {
        if (n++ > 200) {
          out['…'] = '<truncated>';
          break;
        }
        try {
          out[k] = walk(x[k], depth + 1);
        } catch {
          out[k] = '<unreadable>';
        }
      }
      return out;
    }
    return String(x);
  };
  return walk(v, 0);
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    let data = '';
    req.on('data', (c) => {
      data += c;
      if (data.length > 5_000_000) reject(new Error('body too large'));
    });
    req.on('end', () => resolve(data));
    req.on('error', reject);
  });
}

function sendJson(res, status, obj) {
  const body = JSON.stringify(obj);
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body) });
  res.end(body);
}
