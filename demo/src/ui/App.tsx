/* ====================================================================
   App.tsx · app root — wires the real-engine network to the UI
   Ported from the design prototype (asp-app.jsx).
   ==================================================================== */
import React, { useState, useRef, useEffect, useCallback } from 'react';
import { createNetwork } from '../engine/network.ts';
import { NetworkMap, NodePanel, NodeStrip, AddNodeDialog, ConnectPeerDialog, MaxModal } from './components.tsx';
import { Settings, TWEAK_DEFAULTS, type Tweaks } from './settings.tsx';
import { ConfirmHost, confirmDialog } from './confirm.tsx';
import { loadState, loadStateRaw, saveState, saveStateRaw, clearState } from '../persist.ts';

function EmptyState({ onAdd }: any) {
  return (
    <div className="empty">
      <div className="empty-block">
        <span className="corner-bl" /><span className="corner-br" />
        <div className="eyebrow">Agent Sync Protocol</div>
        <h1>An empty mesh.<br />Spin up your first node.</h1>
        <p>
          Each node is a device running ASP — a tiny vault of files, an append-only event log,
          and a live peer-to-peer sync engine. Add a node to create a vault; add more and clone
          them from a peer to watch edits propagate, deterministically, with no commit and no push.
        </p>
        <button className="btn primary" onClick={onAdd} style={{ fontSize: 13.5, padding: '10px 18px' }}>
          <span className="glyph">+</span>Add a new node
        </button>
        <div className="legend">
          <span><span className="dot" style={{ background: 'var(--green)' }} />in sync</span>
          <span><span className="dot" style={{ background: 'var(--cyan)' }} />syncing</span>
          <span><span className="dot" style={{ background: 'var(--red)' }} />offline</span>
          <span><span className="dot" style={{ background: 'var(--amber)' }} />catch-up frame</span>
        </div>
      </div>
    </div>
  );
}

export function App({ api: injected }: { api?: any } = {}) {
  const [t, setT] = useState<Tweaks>(TWEAK_DEFAULTS);
  const setTweak = useCallback((k: keyof Tweaks, v: any) => setT((p) => ({ ...p, [k]: v })), []);
  const engineRef = useRef<any>(null);
  const packetsRef = useRef<any[]>([]);
  const [, setTick] = useState(0);
  const rerender = useCallback(() => setTick((x) => x + 1), []);

  // The engine emits onChange on every step of a sync (each gossip hop, each
  // integrated frame). Rendering synchronously on each one means a render storm
  // during a big catch-up. Coalesce them: at most one render per animation
  // frame. Falls back to setTimeout where rAF is absent (jsdom/headless).
  const rafRef = useRef<number | null>(null);
  const scheduleRender = useCallback(() => {
    if (rafRef.current != null) return;
    const raf =
      typeof requestAnimationFrame !== 'undefined'
        ? requestAnimationFrame
        : (cb: FrameRequestCallback) => setTimeout(() => cb(0), 16) as unknown as number;
    rafRef.current = raf(() => {
      rafRef.current = null;
      setTick((x) => x + 1);
    });
  }, []);
  useEffect(
    () => () => {
      if (rafRef.current != null && typeof cancelAnimationFrame !== 'undefined') cancelAnimationFrame(rafRef.current);
    },
    [],
  );
  const [dialog, setDialog] = useState(false);
  const [connectFor, setConnectFor] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [maximized, setMaximized] = useState<string | null>(null);
  const [toast, setToast] = useState<any>(null);
  const [loaded, setLoaded] = useState(false);
  const toastTimer = useRef<any>(null);
  // Signature of the last persisted state (total rows + edges + tweaks). The
  // autosave skips re-serializing the whole vault when only ephemeral state
  // (status, log lines, packet animation) changed — so a live peer's snapshot
  // churn doesn't trigger a multi-MB save on every frame.
  const lastSaveSig = useRef<string>('');
  const persistSig = useCallback((s: any) => `${s.nodes.reduce((a: number, n: any) => a + n.rowCount, 0)}|${s.edges.length}|${JSON.stringify(t)}`, [t]);

  // Engine event callbacks — stable, so they're identical for the in-page
  // engine (passed at construction) and the worker proxy (set after mount).
  const onPacket = useCallback((pk: any) => { packetsRef.current.push(pk); }, []);
  const onToast = useCallback((msg: string, kind: string) => {
    setToast({ msg, kind });
    if (toastTimer.current) clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(null), 2600);
  }, []);

  // The engine is either injected (the worker-backed NetworkProxy, built in
  // main.tsx and already initialized) or, with no worker (SSR / tests), a plain
  // in-page createNetwork. Either way the UI calls the same method surface.
  if (!engineRef.current) {
    engineRef.current = injected ?? createNetwork({
      latencyMs: t.latencyMs,
      debounceMs: t.debounceMs,
      onChange: () => scheduleRender(),
      onPacket,
      onToast,
    });
  }
  const api = engineRef.current;

  // The proxy can't take callbacks at construction (these refs don't exist
  // yet in main.tsx) — wire them once here.
  useEffect(() => {
    injected?.setCallbacks({ onChange: scheduleRender, onPacket, onToast });
  }, [injected, scheduleRender, onPacket, onToast]);

  useEffect(() => { api.setConfig({ latencyMs: t.latencyMs, debounceMs: t.debounceMs }); }, [t.latencyMs, t.debounceMs]);
  useEffect(() => { document.documentElement.style.setProperty('--cyan', t.accent); }, [t.accent]);
  useEffect(() => { document.body.style.backgroundImage = t.blueprint ? '' : 'none'; }, [t.blueprint]);
  useEffect(() => { const id = setTimeout(() => api.clearFresh(), 1200); return () => clearTimeout(id); });

  // Persistence: restore the mesh from OPFS once on load, then autosave
  // (debounced) on every change. Degrades to a clean session where OPFS is
  // unavailable (see persist.ts).
  useEffect(() => {
    (async () => {
      try {
        // Hand the worker the raw OPFS text so it parses the multi-MB document
        // once, off the main thread (the in-process fallback parses here).
        const raw = await loadStateRaw();
        let tweaks: any = null;
        if (raw && injected) {
          tweaks = await injected.restore(raw);
        } else if (raw) {
          const s = JSON.parse(raw);
          if (s?.nodes?.length) { api.restore(s); tweaks = s.tweaks ?? null; }
        }
        if (api.snapshot().nodes.length) {
          if (tweaks) setT((prev) => ({ ...prev, ...tweaks }));
          const first = api.snapshot().nodes[0];
          if (first) setSelected(first.id);
          lastSaveSig.current = persistSig(api.snapshot()); // don't re-save what we just loaded
          rerender();
        }
      } finally {
        setLoaded(true);
      }
    })();
  }, []);
  useEffect(() => {
    if (!loaded) return;
    const sig = persistSig(api.snapshot());
    if (sig === lastSaveSig.current) return; // nothing persistent changed — skip the save
    const id = setTimeout(() => {
      lastSaveSig.current = sig;
      void (async () => {
        // The worker builds the final JSON string (off the main thread); we just
        // stream it to disk. Falls back to main-thread serialize with no worker.
        if (injected) await saveStateRaw(await injected.serialize(t));
        else await saveState({ tweaks: t, ...api.serialize() });
      })();
    }, 1500);
    return () => clearTimeout(id);
  });

  const snap = api.snapshot();

  // The engine-derived status rides the snapshot (node.status), computed in the
  // worker. Here we only overlay the "frames in flight" bit, which is pure
  // main-thread packet-animation timing. Closes over `snap` so the NetworkMap's
  // own per-frame re-render reflects live packet flight without an engine call.
  const statusFor = (nodeId: string) => {
    const node = snap.nodes.find((n: any) => n.id === nodeId);
    const base = node?.status ?? { kind: 'solo', label: '—', note: '' };
    const now = performance.now();
    let inflight = false;
    for (const pk of packetsRef.current) {
      if (now - pk.started < pk.dur && (pk.fromId === nodeId || pk.toId === nodeId)) { inflight = true; break; }
    }
    if (inflight && base.kind === 'insync') return { kind: 'syncing', label: 'Syncing', note: 'frames in flight' };
    return base;
  };

  async function addNode(opts: any) { const id = await Promise.resolve(api.addNode(opts)); setDialog(false); if (!selected) setSelected(id); }
  async function removeNode(id: string) {
    const node = snap.nodes.find((n: any) => n.id === id);
    const ok = await confirmDialog({
      title: `Remove node “${node?.name ?? id}”?`,
      message: <>The node leaves the mesh and its in-page vault is discarded. Other nodes keep their copies.</>,
      confirmLabel: 'Remove node', danger: true,
    });
    if (!ok) return;
    api.removeNode(id);
    if (maximized === id) setMaximized(null);
    if (selected === id) { const left = snap.nodes.find((n: any) => n.id !== id); setSelected(left ? left.id : null); }
  }
  async function reset() {
    const ok = await confirmDialog({
      title: 'Reset the whole mesh?',
      message: <>All nodes, event logs and vault state are cleared. This cannot be undone.</>,
      confirmLabel: 'Reset everything', danger: true,
    });
    if (!ok) return;
    void clearState();
    packetsRef.current.length = 0;
    await Promise.resolve(api.reset());
    setSelected(null); rerender();
  }
  function doConnect(url: string, authKey?: string) {
    if (connectFor) void api.connectPeer(connectFor, url, authKey);
    setConnectFor(null);
  }

  const nodes = snap.nodes;
  const totalRows = nodes.reduce((a: number, n: any) => a + n.rowCount, 0);
  const sel = selected && nodes.find((n: any) => n.id === selected) ? selected : (nodes[0] && nodes[0].id);
  const layoutClass = t.layout === 'focus' ? 'layout-focus' : t.layout === 'rows' ? 'layout-rows' : `cols-${Math.min(nodes.length, 3)}`;

  return (
    <div className="app">
      <div className="topbar">
        <div className="brand">
          <span className="mark"><span className="tick" />ASP</span>
          <span className="sub">Agent Sync Protocol · p2p sync demo</span>
        </div>
        <div className="spacer" />
        {nodes.length > 0 && (
          <>
            <div className="topbar-stat"><span>nodes</span><b>{nodes.length}</b></div>
            <div className="topbar-stat"><span>log rows</span><b>{totalRows}</b></div>
            <button className="btn ghost tiny" onClick={reset} title="clear everything">Reset</button>
          </>
        )}
        <button className="btn primary" onClick={() => setDialog(true)}>
          <span className="glyph">+</span>Add node
        </button>
      </div>

      {nodes.length === 0 ? (
        <EmptyState onAdd={() => setDialog(true)} />
      ) : (
        <>
          {t.showMap && <NetworkMap snap={snap} packetsRef={packetsRef} selected={sel} onSelect={setSelected} statusFor={statusFor} />}
          {t.layout === 'focus' ? (
            <div className="canvas layout-focus">
              {(() => {
                const focusNode = nodes.find((n: any) => n.id === sel) || nodes[0];
                return <NodePanel key={focusNode.id} snap={focusNode} api={api} status={statusFor(focusNode.id)} extraClass="is-focus" onMaximize={setMaximized} onColumns={() => setTweak('layout', 'columns')} onRemove={removeNode} onConnect={setConnectFor} />;
              })()}
              <div className="focus-rail">
                {nodes.map((n: any) => (
                  <NodeStrip key={n.id} snap={n} status={statusFor(n.id)} selected={n.id === sel} onClick={() => setSelected(n.id)} />
                ))}
              </div>
            </div>
          ) : (
            <div className={`canvas ${layoutClass}`}
              style={t.layout === 'columns' ? { gridTemplateColumns: `repeat(${Math.min(nodes.length, 3)}, minmax(0,1fr))` } : undefined}>
              {nodes.map((n: any) => (
                <NodePanel key={n.id} snap={n} api={api} status={statusFor(n.id)}
                  onMaximize={setMaximized} onColumns={t.layout !== 'columns' ? () => setTweak('layout', 'columns') : undefined} onRemove={removeNode} onConnect={setConnectFor} />
              ))}
            </div>
          )}
        </>
      )}

      {dialog && <AddNodeDialog snap={snap} onCancel={() => setDialog(false)} onAdd={addNode} />}
      {connectFor && (() => {
        const n = nodes.find((x: any) => x.id === connectFor);
        return n ? <ConnectPeerDialog snap={n} onCancel={() => setConnectFor(null)} onConnect={doConnect} onDisconnect={(id: string) => api.disconnectPeer(id)} /> : null;
      })()}
      {maximized && (() => {
        const n = nodes.find((x: any) => x.id === maximized);
        return n ? <MaxModal snap={n} api={api} status={statusFor(n.id)} onClose={() => setMaximized(null)} onConnect={setConnectFor} /> : null;
      })()}
      {toast && <div className="toast-wrap"><div className="toast"><span className="accent">⚠ </span>{toast.msg}</div></div>}
      <ConfirmHost />

      <Settings t={t} set={setTweak} />
    </div>
  );
}
