// Regression: an INCOMPLETE catch-up must fail, not silently succeed.
//
// On a flaky link (mobile) the catch-up socket can drop mid-stream — before the
// peer sends its explicit `Synced` completion. The old `sync` resolved that as
// success with a PARTIAL pull; the caller then reconciled its disk against the
// partial engine, minting brand-new ids for every not-yet-received file, which
// collided with the peer's ids → the whole vault duplicated (the mobile dup
// loop). Completion now REQUIRES `Synced`: a close/stall before it rejects, so
// the caller never reconciles a partial pull.
import { expect, test } from 'bun:test';
import { Vault } from '../src/index.ts';

test('sync rejects when the peer closes mid-catch-up before Synced', async () => {
  // Upgrade the WebSocket (so the client passes the connect phase and enters
  // catch-up), then close shortly after WITHOUT ever sending `Synced`.
  const server = Bun.serve({
    port: 0,
    fetch(req, srv) {
      return srv.upgrade(req) ? undefined : new Response('expected websocket', { status: 426 });
    },
    websocket: {
      open(ws) {
        setTimeout(() => {
          try {
            ws.close();
          } catch {}
        }, 50);
      },
      message() {
        /* ignore the client's Hello — never reply, never send Synced */
      },
    },
  });
  try {
    const v = new Vault(new Uint8Array(32).fill(1), '');
    // Must REJECT (old behavior: resolve with a partial count). Short timeouts so
    // the test is fast regardless of which failure edge fires.
    await expect(v.sync(`ws://localhost:${server.port}`, { idleMs: 400, connectMs: 2000, timeoutMs: 3000 })).rejects.toThrow();
  } finally {
    server.stop(true);
  }
});

test('cancel() during a hanging connect makes sync reject, and is a no-op when idle', async () => {
  // A server that upgrades but never sends anything → the client hangs in
  // catch-up. cancel() must abort it (reject), giving a UI an escape hatch.
  const server = Bun.serve({
    port: 0,
    fetch(req, srv) {
      return srv.upgrade(req) ? undefined : new Response('expected websocket', { status: 426 });
    },
    websocket: { open() {}, message() {} },
  });
  try {
    const v = new Vault(new Uint8Array(32).fill(2), '');
    v.cancel(); // no-op when nothing is in flight — must not throw
    const p = v.sync(`ws://localhost:${server.port}`, { idleMs: 60000, connectMs: 60000, timeoutMs: 60000 });
    await new Promise((r) => setTimeout(r, 100)); // let it reach the catch-up phase
    v.cancel();
    await expect(p).rejects.toThrow(/cancel/i);
  } finally {
    server.stop(true);
  }
});
