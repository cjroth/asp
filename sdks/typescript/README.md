# @asp/sdk

The TypeScript SDK for the **Agent Sync Protocol** — a thin shim over the one
Rust engine (`asp-core`) compiled to WebAssembly (`asp-wasm`). **One engine
everywhere:** a TS/wasm node computes byte-identical state to the native `asp`
daemon — the fold, 3-way merge, identity, and the sans-IO replication `Session`
all run in wasm, not reimplemented in TypeScript.

```ts
import { Vault } from '@asp/sdk';

const v = new Vault(seed /* 32 bytes */, '' /* adopt the peer's vault on connect */);
v.writeFile('notes/todo.md', 'buy milk\n');
await v.sync('ws://hub:9000', { authKey: 'SECRET' }); // handshake + catch-up + converge
console.log(v.readTextFile('notes/todo.md'));
```

## Build & test

```sh
bun run build:wasm   # wasm-pack build → crates/asp-wasm/pkg (+ pkg-web)
bun test             # conformance (wasm == native vectors) + parity (SDK ⇄ real asp)
```

- **`test/conformance.test.ts`** asserts the wasm engine's identity, content
  hashing, deterministic fold, and 3-way merge are **byte-identical** to native,
  against `test-vectors.json` (regenerate with
  `cargo run -p asp-core --example gen_vectors > test-vectors.json`).
- **`test/parity.test.ts`** spawns the real `asp` binary as a listening relay and
  drives a wasm node through the handshake + version-vector catch-up, asserting
  **bidirectional convergence** (including a cross-surface concurrent 3-way merge).

## API

`new Vault(seed, vaultId?)` · `writeFile` · `readFile` / `readTextFile` ·
`deleteFile` · `renameFile` · `commitFiles` · `files()` · `sync(url, {authKey})` ·
`vaultId()` · `nodeId()` / `nodeSsh()`. Low-level conformance helpers:
`foldFiles`, `merge3Bytes`, `contentHash`, `nodeIdHex`, `sshPubkey`, `merkleIdOf`.
