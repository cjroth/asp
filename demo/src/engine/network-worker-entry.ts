/* ====================================================================
   network-worker-entry.ts · the engine Web Worker (demo)
   --------------------------------------------------------------------
   Bundled on its own to a standalone IIFE string (build.mjs Phase 1),
   inlined into the main bundle (`__ASP_NETWORK_WORKER__`) and started as
   a Blob Worker — no separate file to ship, exactly like the Obsidian
   plugin's engine worker. It hosts the WHOLE mesh sim: every WasmEngine,
   the gossip routing, the live ws:// peers. The heavy synchronous engine
   work (integrate/fold/merge over a large catch-up) runs HERE, so the
   renderer thread never freezes during a sync.

   The main thread talks to it through NetworkProxy: `cmd` messages are
   api method calls; `snapshot`/`packet`/`toast` are pushed back out.
   ==================================================================== */
import { initAsp } from '../../../sdks/typescript/src/index.ts';
import { type ASPNetwork, createNetwork } from './network.ts';

const scope = self as unknown as {
  postMessage(m: unknown): void;
  onmessage: ((ev: MessageEvent) => void) | null;
  addEventListener: (name: string, h: (e: { reason?: unknown }) => void) => void;
};

// A silently-dead worker is the worst failure mode — re-raise unhandled
// rejections so they reach the host's worker.onerror instead of vanishing.
scope.addEventListener('unhandledrejection', (e) => {
  const reason = e.reason instanceof Error ? e.reason : new Error(String(e.reason));
  queueMicrotask(() => {
    throw reason;
  });
});

function post(m: unknown): void {
  scope.postMessage(m);
}

let net: ASPNetwork | null = null;

// Coalesce a burst of onChange callbacks (one per gossip hop / integrated
// frame) into a single snapshot post per macrotask — the wire analogue of the
// main thread's rAF render-coalescing.
let snapScheduled = false;
function flushSnapshot(): void {
  snapScheduled = false;
  if (net) post({ kind: 'snapshot', snap: net.snapshot() });
}
function scheduleSnapshot(): void {
  if (snapScheduled) return;
  snapScheduled = true;
  setTimeout(flushSnapshot, 0);
}

function ensureNet(): void {
  net = createNetwork({
    onChange: () => scheduleSnapshot(),
    onPacket: (pk) => post({ kind: 'packet', pk }),
    onToast: (msg, kind) => post({ kind: 'toast', msg, tone: kind }),
  });
}

// Reply ops whose caller awaits the result; for these we flush the snapshot
// BEFORE replying, so the main thread's cached snapshot already reflects the
// mutation by the time the await resolves (addNode → node visible, restore →
// restored tree, connectPeer → converged state).
const REPLY_OPS = new Set(['init', 'addNode', 'connectPeer', 'serialize', 'restore', 'reset']);

scope.onmessage = (ev: MessageEvent) => {
  void handle(ev.data as { kind: string; id: number; op: string; args: any[] });
};

async function handle(m: { kind: string; id: number; op: string; args: any[] }): Promise<void> {
  if (m.kind !== 'cmd') return;
  const { id, op, args } = m;
  const wantsReply = id !== 0;
  try {
    let value: any;
    if (op === 'init') {
      await initAsp(args[0]); // instantiate the inlined wasm inside the worker
      ensureNet();
    } else if (op === 'reset') {
      if (!net) ensureNet();
      else net.reset();
    } else if (op === 'serialize') {
      // Build the final OPFS document HERE and return a string — the main
      // thread never stringifies (or structured-clones) the multi-MB vault.
      if (!net) throw new Error('network worker: not initialized');
      value = JSON.stringify({ tweaks: args[0], ...net.serialize() });
    } else if (op === 'restore') {
      // Parse the raw OPFS text once, off the main thread; hand back tweaks.
      if (!net) throw new Error('network worker: not initialized');
      const state = JSON.parse(args[0]);
      net.restore(state);
      value = state.tweaks ?? null;
    } else {
      if (!net) throw new Error('network worker: not initialized');
      let r = (net as any)[op](...args);
      if (r && typeof r.then === 'function') r = await r;
      value = r;
    }
    if (wantsReply && REPLY_OPS.has(op)) flushSnapshot();
    if (wantsReply) post({ kind: 'reply', id, ok: true, value });
  } catch (e) {
    if (wantsReply) post({ kind: 'reply', id, ok: false, error: String(e) });
  }
}
