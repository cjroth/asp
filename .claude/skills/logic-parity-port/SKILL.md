---
name: logic-parity-port
description: Port pure logic from an existing reference implementation (TS/JS, Python, etc.) into another language with PROVABLE behavioral parity — by mirroring the reference's own test fixtures 1:1 and verifying in a fast isolated build before integrating. Use when reimplementing, porting, or rewriting an app/library where the new code must match existing behavior exactly (a native rewrite, a cross-language port, a v2 that can't regress). Turns "I think it matches" into a green test suite that mirrors the original's.
---

# Port logic with provable parity

When you reimplement existing logic, the risk is subtle behavioral drift. The fix:
don't just port the code — **port its tests too**, case-for-case, so equivalence is
machine-checked, not asserted.

## The loop
1. **Pick pure-logic units first.** Port the framework-free modules (parsing,
   formatting, data transforms, geometry, sorting, state reducers) before any UI/IO.
   They carry most of the behavior and are the easiest to verify exactly.
2. **Read the reference module AND its tests.** The original's test file is the spec
   — it encodes the exact edge cases (empty input, unicode, ordering, off-by-one,
   error cases). Port those assertions verbatim into the target language's test
   framework, one test per original test.
3. **Match semantics, not just signatures.** Replicate the quirks the tests pin:
   - locale/collation (`localeCompare({numeric,sensitivity})` → a real natural,
     case-insensitive comparator),
   - JS number/bit ops (`>>> 0` → `u32` wrapping; `charCodeAt` → UTF-16 units),
     hash functions must match bit-for-bit if values cross the boundary,
   - string ops (`split(/\s+/)` keeps leading/trailing empties — `split_whitespace`
     does NOT; `lastIndexOf`/slice edge cases),
   - inject impure inputs (clock, RNG) as parameters so tests are deterministic
     (`rel_time(ts, now)` not `rel_time(ts)` reading the system clock).
4. **Decouple from the framework.** Take plain inputs (`(path, is_dir)` tuples, a
   `&str`) instead of the original's framework types, so the unit is testable in
   isolation and reusable.
5. **Verify in a throwaway crate/project FIRST.** Before wiring a module into a big,
   slow-building app, compile + test it alone:
   ```bash
   mkdir -p /tmp/parity/src && cd /tmp/parity
   printf '[package]\nname="p"\nversion="0.0.0"\nedition="2021"\n[lib]\npath="src/lib.rs"\n' > Cargo.toml
   cp /path/to/ported/module.rs src/module.rs
   echo 'pub mod module;' > src/lib.rs
   cargo test            # green in <1s, no waiting on the real app to build
   ```
   Iterate here until green, then copy into the real crate and add `mod module;`.
6. **Then integrate + re-run the full suite.** Wire the verified module in; keep its
   tests with it.

## What this buys
- Each ported module lands with a test suite that mirrors the original's — drift is
  caught immediately, and the suite documents the contract.
- The throwaway-crate step removes the slow build from the inner loop, so porting is
  fast even when the real app takes minutes to compile.
- When you later refactor or extend, the parity tests stop you from silently
  changing behavior the original guaranteed.

## Scope note
This is for PURE logic. For UI/rendering/IO, use integration tests against the real
backend + (where possible) snapshot/visual tests — parity-by-fixtures is the wrong
tool there. Port the deterministic core this way; verify the shell separately.
