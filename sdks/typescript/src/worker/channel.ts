// The thin transport seam under the engine-worker protocol. A `Port` is just
// "post a message / receive messages" — the minimal slice of `Worker` /
// `DedicatedWorkerGlobalScope` the protocol needs. Production wraps a real
// `Worker` (main side) and `self` (worker side). Tests use `linkedPorts()` to
// wire the two halves in-process, so the whole WorkerVault ⇄ EngineWorkerHost
// stack runs under `bun test` with no real Worker / DOM.

/** One end of a message channel. `TOut` is what this end sends, `TIn` what it
 * receives. */
export interface Port<TOut, TIn> {
  post(msg: TOut): void;
  /** Register the (single) message handler. Replaces any previous one. */
  onMessage(handler: (msg: TIn) => void): void;
}

/** Wrap a real `Worker` (the main-thread end). */
export function workerPort<TOut, TIn>(worker: Worker): Port<TOut, TIn> {
  return {
    post: (msg) => worker.postMessage(msg),
    onMessage: (handler) => {
      worker.onmessage = (ev: MessageEvent) => handler(ev.data as TIn);
    },
  };
}

/** Wrap the worker global scope (`self`, the worker-thread end). */
export function selfPort<TOut, TIn>(scope: {
  postMessage(m: unknown): void;
  onmessage: ((ev: MessageEvent) => void) | null;
}): Port<TOut, TIn> {
  return {
    post: (msg) => scope.postMessage(msg),
    onMessage: (handler) => {
      scope.onmessage = (ev: MessageEvent) => handler(ev.data as TIn);
    },
  };
}

/**
 * Two in-process ports wired back-to-back — the test double for a real `Worker`
 * boundary. Delivery is deferred through `queueMicrotask` so it mimics
 * `postMessage`'s always-async dispatch (a handler never observes a message
 * synchronously inside its own `post`), which the request/reply correlation
 * relies on. Messages posted before the far end registers its handler are
 * queued and flushed on registration.
 */
export function linkedPorts<A, B>(): [Port<A, B>, Port<B, A>] {
  let handlerA: ((msg: B) => void) | null = null;
  let handlerB: ((msg: A) => void) | null = null;
  const pendingForA: B[] = [];
  const pendingForB: A[] = [];

  const portA: Port<A, B> = {
    post: (msg) =>
      queueMicrotask(() => {
        if (handlerB) handlerB(msg);
        else pendingForB.push(msg);
      }),
    onMessage: (handler) => {
      handlerA = handler;
      const drain = pendingForA.splice(0);
      for (const m of drain) queueMicrotask(() => handler(m));
    },
  };
  const portB: Port<B, A> = {
    post: (msg) =>
      queueMicrotask(() => {
        if (handlerA) handlerA(msg);
        else pendingForA.push(msg);
      }),
    onMessage: (handler) => {
      handlerB = handler;
      const drain = pendingForB.splice(0);
      for (const m of drain) queueMicrotask(() => handler(m));
    },
  };
  return [portA, portB];
}
