// Completion REQUIRES the peer's `Synced`: a sync that fails or drops before the
// catch-up completes must REJECT, never resolve with a silent partial pull (which
// would make the caller reconcile its disk against a partial engine and mint
// brand-new ids for every not-yet-received file → the whole vault duplicates, the
// mobile dup loop). With iroh the whole connect+drive lives in the wasm engine
// (asp-core::iroh_wasm), which returns an error unless it reached `Synced`.
import { expect, test } from 'bun:test';
import { Vault } from '../src/index.ts';

test('sync rejects on an empty ticket', async () => {
  const v = new Vault(new Uint8Array(32).fill(1), '');
  await expect(v.sync('')).rejects.toThrow();
});

test('sync rejects on a malformed ticket', async () => {
  // Parsed before any network/bind, so this fails fast.
  const v = new Vault(new Uint8Array(32).fill(1), '');
  await expect(v.sync('not-a-valid-iroh-ticket')).rejects.toThrow();
});

test('cancel() is a safe no-op (iroh drives the connection inside the engine)', () => {
  const v = new Vault(new Uint8Array(32).fill(2), '');
  expect(() => v.cancel()).not.toThrow();
});
