# AGENTS.md — Context Desktop (asp) frontend

Guidance for AI agents working in `desktop/`. The app is a React + Tauri vault
editor; the backend is reached only through `src/lib/api.ts` (Tauri commands on
desktop, wasm+OPFS on web). No protocol logic lives in the frontend.

## Testing is mandatory — TDD, and it's not done until tests pass

- **Test-driven.** Write or update tests alongside (ideally before) the change.
  A feature without tests is not complete.
- **All tests must pass.** Run the full suite, not just the file you touched:
  ```bash
  bun run test            # all tests (bun test src && bun test bun-isolated)
  bun run typecheck       # tsc --noEmit
  ```
- **The suite runs on bun's native test runner** (`bun test`), not vitest: this
  box has only bun, and bun's loader bypasses vitest's relative-module `vi.mock`.
  Tests keep their vitest-style shape via a compat layer:
  - `src/test-shim.ts` re-exports `describe/it/expect/…` from `bun:test` and maps
    `vi.*` (fn/spyOn/clearAllMocks/timers/stubGlobal) onto bun's primitives.
    Import the test API `from './test-shim'` (or `'../test-shim'`), not `'vitest'`.
  - Module mocking uses bun's `mock.module(...)` directly (so the specifier
    resolves relative to the test file). Each mocking file ends with
    `afterAll(() => mock.restore())` so its mocks don't leak — bun shares ONE
    process across files with no isolation.
  - `src/bun-test-preload.ts` (wired in `bunfig.toml`) registers a jsdom DOM +
    the in-memory localStorage / execCommand / `__TAURI_INTERNALS__` setup.
  - `bun-isolated/api.test.ts` runs in its own `bun test` invocation. It exercises
    the REAL `src/lib/api.ts`, which other files `mock.module('./lib/api')`; bun
    applies module mocks by resolved path but restores by specifier, so that one
    file must be isolated to avoid the leaked mock. Keep real-`api.ts` tests there.
- **Aim for 100% test coverage** of the logic you add or change. NOTE: bun's
  coverage (`bun test --coverage`) has no per-file thresholds, so the old
  per-module 100% pins aren't machine-enforced — hold the bar by review instead,
  and never delete assertions to make a run green. Add the missing test instead.

## Conventions

- Backend access goes through `src/lib/api.ts` only. New backend calls: add to
  the `Api` interface, the `tauriApi` map (→ `invoke('command_name', {...})`),
  the web backend (`src/lib/webApi.ts`), the Rust command
  (`src-tauri/src/commands.rs`) and register it in `src-tauri/src/lib.rs`. Cover
  the new `tauriApi` method in `bun-isolated/api.test.ts` (the real-`api.ts` suite).
- The live editor (`LiveEditor.tsx` + `markdown.ts`) keeps a strict 1:1 mapping
  of top-level child `<div>`s ↔ source lines. `readLive`/caret math depend on
  it — preserve this invariant for any rendering change.
- Styling is theme-driven via CSS variables in `styles.css`; don't hardcode
  colors that should follow the theme.
