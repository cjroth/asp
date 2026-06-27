# AGENTS.md — Context Desktop (asp) frontend

Guidance for AI agents working in `desktop/`. The app is a React + Tauri vault
editor; the backend is reached only through `src/lib/api.ts` (Tauri commands on
desktop, wasm+OPFS on web). No protocol logic lives in the frontend.

## Testing is mandatory — TDD, and it's not done until tests pass

- **Test-driven.** Write or update tests alongside (ideally before) the change.
  A feature without tests is not complete.
- **All tests must pass.** Run the full suite, not just the file you touched:
  ```bash
  npx vitest run          # all tests
  npx tsc --noEmit        # typecheck
  npx vitest run --coverage   # tests + coverage thresholds
  ```
- **Aim for 100% test coverage** of the logic you add or change. Pure
  logic/util modules are held at 100% individually (see `vitest.config.ts`
  thresholds) — never let them regress, and prefer to pin new pure modules at
  100% the same way. The view layer (App/components) is exercised end-to-end;
  cover new branches you introduce there too.
- **Never lower a coverage threshold** to make a build pass. Add the missing
  test instead.

## Conventions

- Backend access goes through `src/lib/api.ts` only. New backend calls: add to
  the `Api` interface, the `tauriApi` map (→ `invoke('command_name', {...})`),
  the web backend (`src/lib/webApi.ts`), the Rust command
  (`src-tauri/src/commands.rs`) and register it in `src-tauri/src/lib.rs`. Cover
  the new `tauriApi` method in `src/lib/api.test.ts` (that file is pinned 100%).
- The live editor (`LiveEditor.tsx` + `markdown.ts`) keeps a strict 1:1 mapping
  of top-level child `<div>`s ↔ source lines. `readLive`/caret math depend on
  it — preserve this invariant for any rendering change.
- Styling is theme-driven via CSS variables in `styles.css`; don't hardcode
  colors that should follow the theme.
