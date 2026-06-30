// DEV-ONLY automation bridge. Lets an external script run JavaScript inside this
// running webview — the desktop WKWebView *or* a browser tab — over a WebSocket,
// so tests/agents can drive and observe the real app (clone a vault, edit files,
// read the tree) without GUI clicking or any macOS WKWebView WebDriver.
//
// It is gated on `import.meta.env.DEV`, so it is tree-shaken out of the
// production (`build:web`) bundle entirely and never ships. The webview dials
// OUT to a local bridge server (browsers can't listen), which forwards eval
// requests addressed by surface ('desktop' | 'web').
import { api } from './api';
import { isDesktop } from './platform';

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor as any;

export function startDevBridge(url = 'ws://localhost:17999/ws'): void {
  if (!import.meta.env.DEV) return;
  const surface = isDesktop() ? 'desktop' : 'web';

  const connect = () => {
    let ws: WebSocket;
    try {
      ws = new WebSocket(url);
    } catch {
      setTimeout(connect, 1500);
      return;
    }
    ws.onopen = () => ws.send(JSON.stringify({ type: 'hello', surface }));
    ws.onmessage = async (ev) => {
      let msg: { type?: string; id?: number; code?: string };
      try {
        msg = JSON.parse(String(ev.data));
      } catch {
        return;
      }
      if (msg.type !== 'eval' || typeof msg.code !== 'string') return;
      const reply: { type: string; id?: number; ok: boolean; value?: unknown; error?: string } = { type: 'result', id: msg.id, ok: true };
      try {
        // `api`, `document`, `window` are in scope for the evaluated code.
        const value = await new AsyncFunction('api', 'document', 'window', msg.code)(api, document, window);
        reply.value = value === undefined ? null : value;
      } catch (e) {
        reply.ok = false;
        reply.error = String((e as Error)?.stack || e);
      }
      try {
        ws.send(JSON.stringify(reply));
      } catch {
        ws.send(JSON.stringify({ type: 'result', id: msg.id, ok: reply.ok, value: '[unserializable]', error: reply.error }));
      }
    };
    ws.onclose = () => setTimeout(connect, 1500);
    ws.onerror = () => {
      try {
        ws.close();
      } catch {
        /* ignore */
      }
    };
  };
  connect();
}
