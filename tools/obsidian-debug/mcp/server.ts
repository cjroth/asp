// MCP server — runs INSIDE the OrbStack VM, registered with Claude Code. It is
// the agent-facing surface for debugging the `agent-sync` Obsidian plugin that
// runs in Obsidian on the Mac host.
//
// Two channels, picked per-tool for whichever is cleaner:
//   • Direct filesystem over the OrbStack virtiofs mount — vault file CRUD and
//     swapping in a freshly-built plugin main.js. No round-trip to Obsidian.
//   • HTTP to the ASP Debug Bridge companion plugin (via host.docker.internal)
//     for anything that needs the *running* app: eval, console, plugin reload.
//
// Run: bun run server.ts   (registered via `claude mcp add` — see README).

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { z } from 'zod';
import { exec } from 'node:child_process';
import { promisify } from 'node:util';
import { readFile, writeFile, rm, readdir, mkdir, copyFile, stat } from 'node:fs/promises';
import { join, dirname, resolve } from 'node:path';

const pexec = promisify(exec);

// ---- config (env-overridable; defaults match this VM's setup) -------------
const HOST = process.env.ASP_DEBUG_HOST ?? 'host.docker.internal';
const PORT = Number(process.env.ASP_DEBUG_PORT ?? 28900);
const TOKEN = process.env.ASP_DEBUG_TOKEN ?? 'asp-debug-bridge';
const VAULT = process.env.ASP_VAULT ?? '/mnt/mac/Users/chris/shared/careerbot-sync-2';
const ASP_SRC = process.env.ASP_SRC ?? '/home/chris/asp/plugins/obsidian';
const ASP_PLUGIN_ID = 'agent-sync';
const BASE = `http://${HOST}:${PORT}`;
const pluginDir = join(VAULT, '.obsidian', 'plugins', ASP_PLUGIN_ID);

// ---- bridge helpers (talk to the companion plugin) ------------------------

async function bridge(path: string, init?: RequestInit): Promise<any> {
  const url = `${BASE}${path}`;
  const res = await fetch(url, {
    ...init,
    headers: { 'x-asp-token': TOKEN, 'content-type': 'application/json', ...(init?.headers ?? {}) },
    signal: AbortSignal.timeout(20_000),
  }).catch((e) => {
    throw new Error(
      `cannot reach ASP Debug Bridge at ${BASE} (${e}). Is Obsidian open with the ` +
        `"ASP Debug Bridge" plugin enabled, and is ${HOST} reachable from the VM?`,
    );
  });
  const text = await res.text();
  let json: any;
  try {
    json = JSON.parse(text);
  } catch {
    throw new Error(`bridge returned non-JSON (HTTP ${res.status}): ${text.slice(0, 500)}`);
  }
  return json;
}

/** Eval a JS snippet in the Obsidian renderer; returns the deserialized result
 * or throws with the remote error + stack. */
async function evalRemote(code: string): Promise<any> {
  const r = await bridge('/eval', { method: 'POST', body: JSON.stringify({ code }) });
  if (!r.ok) throw new Error(`${r.error}${r.stack ? `\n${r.stack}` : ''}`);
  return r.result;
}

// ---- result helpers -------------------------------------------------------

const text = (s: string) => ({ content: [{ type: 'text' as const, text: s }] });
const json = (v: unknown) => text(typeof v === 'string' ? v : JSON.stringify(v, null, 2));
const fail = (e: unknown) => ({
  isError: true,
  content: [{ type: 'text' as const, text: `Error: ${e instanceof Error ? e.message : String(e)}` }],
});

/** Resolve a vault-relative path, refusing escapes outside the vault. */
function vaultPath(rel: string): string {
  const abs = resolve(VAULT, rel);
  if (abs !== VAULT && !abs.startsWith(VAULT + '/')) throw new Error(`path escapes vault: ${rel}`);
  return abs;
}

// ---- server ---------------------------------------------------------------

const server = new McpServer({ name: 'asp-obsidian-debug', version: '0.1.0' });

server.tool(
  'obsidian_ping',
  'Health check the ASP Debug Bridge: returns Obsidian version, vault name, and whether the agent-sync plugin is loaded plus its sync state. Use this first to confirm the bridge is reachable.',
  {},
  async () => {
    try {
      return json(await bridge('/ping'));
    } catch (e) {
      return fail(e);
    }
  },
);

server.tool(
  'obsidian_eval',
  'Eval arbitrary JS in the Obsidian renderer against the live `app`. The snippet runs in an async function with `app`, `plugin` (the bridge), and `obsidian` in scope; use `return` to send a value back. Example: `return app.vault.getFiles().length`.',
  { code: z.string().describe('JS to evaluate; may use await and return a value') },
  async ({ code }) => {
    try {
      return json(await evalRemote(code));
    } catch (e) {
      return fail(e);
    }
  },
);

server.tool(
  'obsidian_console',
  'Read the renderer console ring buffer (console.log/info/warn/error/debug plus window error + unhandledrejection events) captured by the bridge since it loaded.',
  {
    level: z.enum(['log', 'info', 'warn', 'error', 'debug']).optional().describe('filter by level'),
    limit: z.number().int().positive().optional().describe('only the last N entries'),
    clear: z.boolean().optional().describe('clear the buffer after reading'),
  },
  async ({ level, limit, clear }) => {
    try {
      const q = new URLSearchParams();
      if (level) q.set('level', level);
      if (limit) q.set('limit', String(limit));
      if (clear) q.set('clear', '1');
      const r = await bridge(`/console?${q}`);
      const lines = (r.entries ?? []).map((e: any) => `[${e.ts}] ${e.level.toUpperCase()} ${e.text}`);
      return text(lines.length ? lines.join('\n') : '(console buffer empty)');
    } catch (e) {
      return fail(e);
    }
  },
);

server.tool(
  'asp_log',
  "Read the agent-sync plugin's own in-app LogBuffer (its structured sync trace) — more useful for asp debugging than the raw console.",
  { clear: z.boolean().optional().describe('clear the asp log after reading') },
  async ({ clear }) => {
    try {
      const out = await evalRemote(
        `const p = app.plugins.plugins['${ASP_PLUGIN_ID}'];` +
          `if (!p) return '(agent-sync not loaded)';` +
          `const t = p.log.toText();` +
          `${clear ? 'p.log.clear();' : ''}` +
          `return t || '(asp log empty)';`,
      );
      return text(out);
    } catch (e) {
      return fail(e);
    }
  },
);

server.tool(
  'asp_state',
  'Dump the agent-sync plugin runtime state: settings, current syncState, device peer id and public key.',
  {},
  async () => {
    try {
      const out = await evalRemote(
        `const p = app.plugins.plugins['${ASP_PLUGIN_ID}'];` +
          `if (!p) return '(agent-sync not loaded)';` +
          `return { syncState: p.syncState, settings: p.settings, ` +
          `peerId: p.peerId?.(), deviceKey: p.deviceKey?.() };`,
      );
      return json(out);
    } catch (e) {
      return fail(e);
    }
  },
);

server.tool(
  'asp_reload',
  'Reload the agent-sync plugin in place (disable then enable) so it picks up a newly deployed main.js. Returns the version that came back up.',
  {},
  async () => {
    try {
      const out = await evalRemote(
        `await app.plugins.disablePlugin('${ASP_PLUGIN_ID}');` +
          `await app.plugins.enablePlugin('${ASP_PLUGIN_ID}');` +
          `const p = app.plugins.plugins['${ASP_PLUGIN_ID}'];` +
          `return { reloaded: !!p, version: p?.manifest?.version };`,
      );
      return json(out);
    } catch (e) {
      return fail(e);
    }
  },
);

server.tool(
  'asp_reset',
  'Reset agent-sync. Default (config reset) calls resetSyncConfig(): forgets the hub URL but KEEPS vault history and device identity. With wipe=true it deletes the plugin data.json (losing the device seed/identity) and reloads — a from-scratch reset.',
  { wipe: z.boolean().optional().describe('true = delete data.json (full reset, new identity)') },
  async ({ wipe }) => {
    try {
      if (wipe) {
        await rm(join(pluginDir, 'data.json'), { force: true });
        const out = await evalRemote(
          `await app.plugins.disablePlugin('${ASP_PLUGIN_ID}');` +
            `await app.plugins.enablePlugin('${ASP_PLUGIN_ID}');` +
            `return 'data.json wiped, plugin reloaded with fresh identity';`,
        );
        return text(out);
      }
      const out = await evalRemote(
        `const p = app.plugins.plugins['${ASP_PLUGIN_ID}'];` +
          `if (!p) return '(agent-sync not loaded)';` +
          `await p.resetSyncConfig();` +
          `return 'sync config reset (remote forgotten; history + identity kept)';`,
      );
      return text(out);
    } catch (e) {
      return fail(e);
    }
  },
);

server.tool(
  'asp_update',
  'Rebuild the agent-sync plugin from source in the VM (bun run build), copy the fresh main.js + manifest.json into the vault, then reload it in Obsidian. The full edit→test loop in one call.',
  { skipBuild: z.boolean().optional().describe('skip the build; just deploy the existing main.js') },
  async ({ skipBuild }) => {
    try {
      const log: string[] = [];
      if (!skipBuild) {
        const { stdout, stderr } = await pexec('bun run build', { cwd: ASP_SRC, timeout: 300_000 });
        log.push('--- build ---', stdout.trim(), stderr.trim());
      }
      await mkdir(pluginDir, { recursive: true });
      await copyFile(join(ASP_SRC, 'main.js'), join(pluginDir, 'main.js'));
      await copyFile(join(ASP_SRC, 'manifest.json'), join(pluginDir, 'manifest.json'));
      const sz = (await stat(join(pluginDir, 'main.js'))).size;
      log.push(`--- deployed main.js (${(sz / 1024).toFixed(0)} KB) → ${pluginDir} ---`);
      const out = await evalRemote(
        `await app.plugins.disablePlugin('${ASP_PLUGIN_ID}');` +
          `await app.plugins.enablePlugin('${ASP_PLUGIN_ID}');` +
          `const p = app.plugins.plugins['${ASP_PLUGIN_ID}'];` +
          `return { reloaded: !!p, version: p?.manifest?.version };`,
      );
      log.push('--- reload ---', JSON.stringify(out));
      return text(log.filter(Boolean).join('\n'));
    } catch (e) {
      return fail(e);
    }
  },
);

server.tool(
  'vault_write',
  'Create or overwrite a file in the vault (path is vault-relative). Writes directly over the OrbStack mount; Obsidian sees it and fires its create/modify event, which triggers an asp sync.',
  {
    path: z.string().describe('vault-relative path, e.g. "notes/test.md"'),
    content: z.string().describe('file contents'),
  },
  async ({ path, content }) => {
    try {
      const abs = vaultPath(path);
      await mkdir(dirname(abs), { recursive: true });
      await writeFile(abs, content);
      return text(`wrote ${path} (${Buffer.byteLength(content)} bytes)`);
    } catch (e) {
      return fail(e);
    }
  },
);

server.tool(
  'vault_delete',
  'Delete a file (or directory) from the vault (path is vault-relative). Obsidian sees it and fires its delete event, triggering an asp sync.',
  { path: z.string().describe('vault-relative path') },
  async ({ path }) => {
    try {
      await rm(vaultPath(path), { recursive: true, force: true });
      return text(`deleted ${path}`);
    } catch (e) {
      return fail(e);
    }
  },
);

server.tool(
  'vault_read',
  'Read a file from the vault (path is vault-relative).',
  { path: z.string().describe('vault-relative path') },
  async ({ path }) => {
    try {
      return text(await readFile(vaultPath(path), 'utf8'));
    } catch (e) {
      return fail(e);
    }
  },
);

server.tool(
  'vault_list',
  'List entries in a vault directory (vault-relative; defaults to vault root). Marks directories with a trailing slash.',
  { dir: z.string().optional().describe('vault-relative directory; default root') },
  async ({ dir }) => {
    try {
      const abs = vaultPath(dir ?? '.');
      const ents = await readdir(abs, { withFileTypes: true });
      const lines = ents.map((e) => (e.isDirectory() ? `${e.name}/` : e.name)).sort();
      return text(lines.join('\n') || '(empty)');
    } catch (e) {
      return fail(e);
    }
  },
);

await server.connect(new StdioServerTransport());
