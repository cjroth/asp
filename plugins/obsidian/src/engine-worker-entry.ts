// Entry point for the engine Web Worker.
//
// esbuild bundles this file on its own into a standalone IIFE string, which the
// main bundle inlines (`__ASP_ENGINE_WORKER__`) and starts as a Blob Worker — no
// separate file to ship, mobile-WebView-safe (the same constraint that drives
// wasm inlining). All it does is stand up an `EngineWorkerHost` bound to the
// worker global scope: the host owns the real wasm engine + WebSocket transport,
// so the heavy synchronous engine work runs here, off the renderer thread, and
// the Obsidian UI never freezes during a sync.

import {
  EngineWorkerHost,
  type FromWorker,
  selfPort,
  type ToWorker,
} from '../../../sdks/typescript/src/index.ts';

// `self` in a dedicated worker is the `DedicatedWorkerGlobalScope` — it has the
// `postMessage` + `onmessage` slice `selfPort` needs.
const scope = self as unknown as {
  postMessage(m: unknown): void;
  onmessage: ((ev: MessageEvent) => void) | null;
  addEventListener: (name: string, h: (e: { reason?: unknown }) => void) => void;
};

// Unhandled rejections don't auto-propagate to the host's `worker.onerror`;
// re-raise them so an operator sees them instead of a silently-dead worker.
scope.addEventListener('unhandledrejection', (e) => {
  const reason = e.reason instanceof Error ? e.reason : new Error(String(e.reason));
  queueMicrotask(() => {
    throw reason;
  });
});

new EngineWorkerHost(selfPort<FromWorker, ToWorker>(scope));
