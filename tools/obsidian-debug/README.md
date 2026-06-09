# ASP Obsidian debug bridge

A dev-only bridge that lets Claude Code (running **inside the OrbStack VM**) drive
and inspect the `agent-sync` plugin in **Obsidian on the Mac host** — reload the
plugin, read the console, eval JS against the live `app`, and create/delete vault
files to trigger syncs.

This is the Obsidian analogue of `dev-vm/mac/firefox-mcp.sh`, but it needs **no
socat relay**: the companion plugin listens on the Mac's loopback and the VM
dials *out* to it via OrbStack's `host.docker.internal`.

## Architecture

```
VM (Claude Code)                         Mac (Obsidian)
┌─────────────────────────┐              ┌────────────────────────────────┐
│ asp-obsidian MCP server │  HTTP        │ "ASP Debug Bridge" plugin       │
│  (mcp/server.ts)        │ ───────────▶ │  http://127.0.0.1:28900         │
│                         │ host.docker  │   POST /eval   (app/plugin/obs) │
│  obsidian_eval/ping     │ .internal    │   GET  /console (ring buffer)   │
│  asp_log/state/reload   │              │   GET  /ping                    │
│  asp_reset/update       │              │                                 │
│                         │              │ drives → agent-sync plugin      │
│  vault_* ───────────────┼──────────────▶ vault files (virtiofs mount)   │
└─────────────────────────┘  /mnt/mac…  └────────────────────────────────┘
```

Two channels, chosen per tool:

- **Direct filesystem** over the virtiofs mount (`/mnt/mac/.../<vault>`) for vault
  file CRUD and deploying a freshly-built `main.js`. No round-trip to Obsidian.
- **HTTP to the companion plugin** for anything needing the *running* app: eval,
  console, plugin reload. Everything ultimately reduces to `POST /eval`; the one
  thing eval can't do retroactively is read past console output, so the plugin
  keeps a console ring buffer.

## Install

```bash
tools/obsidian-debug/install.sh
```

Then **quit & reopen Obsidian** on the Mac (or toggle *ASP Debug Bridge* in
Settings → Community plugins) so the bridge starts. Confirm from Claude with
`obsidian_ping`.

Defaults target the `careerbot-sync-2` vault (the one with `agent-sync`
installed). Override with `ASP_VAULT`, `ASP_DEBUG_PORT`, `ASP_DEBUG_TOKEN`
(keep them consistent across `install.sh`, the MCP, and the plugin's env).

## Tools

| Tool | What it does |
| --- | --- |
| `obsidian_ping` | Bridge health: Obsidian version, vault, agent-sync presence + sync state |
| `obsidian_eval` | Eval JS in the renderer (`app`, `plugin`, `obsidian` in scope; `return` a value) |
| `obsidian_console` | Read the renderer console + error ring buffer (filter by level, clear) |
| `asp_log` | Read agent-sync's own `LogBuffer` sync trace |
| `asp_state` | Dump agent-sync settings, syncState, peer id, device key |
| `asp_reload` | Disable→enable agent-sync (pick up a new `main.js`) |
| `asp_reset` | `resetSyncConfig()`; `wipe=true` deletes `data.json` for a from-scratch reset |
| `asp_update` | `bun run build` → copy `main.js`+manifest into the vault → reload |
| `vault_write` / `vault_delete` / `vault_read` / `vault_list` | Vault file ops over the mount |

## Notes

- **Dev-only / never ship.** The bridge is an eval endpoint on loopback, guarded
  only by a shared token. It's `isDesktopOnly` (uses Node `http`) and lives here,
  not in the shipped `plugins/obsidian`.
- Updating the **bridge** plugin itself: re-run `install.sh` and reload it from
  Obsidian (it can't reliably reload itself mid-request).
- If `obsidian_ping` can't connect: confirm Obsidian is open with the bridge
  enabled, and that `host.docker.internal` resolves in the VM
  (`getent hosts host.docker.internal`).
