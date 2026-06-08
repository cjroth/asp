/* ====================================================================
   App.tsx · app root — wires the real-engine network to the UI
   Ported from the design prototype (asp-app.jsx).
   ==================================================================== */
import React, { useState, useRef, useEffect, useCallback } from 'react';
import { createNetwork } from '../engine/network.ts';
import { NetworkMap, NodePanel, NodeStrip, AddNodeDialog } from './components.tsx';
import { Settings, TWEAK_DEFAULTS, type Tweaks } from './settings.tsx';

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

export function App() {
  const [t, setT] = useState<Tweaks>(TWEAK_DEFAULTS);
  const setTweak = useCallback((k: keyof Tweaks, v: any) => setT((p) => ({ ...p, [k]: v })), []);
  const engineRef = useRef<any>(null);
  const packetsRef = useRef<any[]>([]);
  const [, setTick] = useState(0);
  const rerender = useCallback(() => setTick((x) => x + 1), []);
  const [dialog, setDialog] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [toast, setToast] = useState<any>(null);
  const toastTimer = useRef<any>(null);

  function buildEngine() {
    return createNetwork({
      latencyMs: t.latencyMs,
      debounceMs: t.debounceMs,
      onChange: () => rerender(),
      onPacket: (pk) => { packetsRef.current.push(pk); },
      onToast: (msg, kind) => {
        setToast({ msg, kind });
        if (toastTimer.current) clearTimeout(toastTimer.current);
        toastTimer.current = setTimeout(() => setToast(null), 2600);
      },
    });
  }
  if (!engineRef.current) engineRef.current = buildEngine();
  const api = engineRef.current;

  useEffect(() => { api.setConfig({ latencyMs: t.latencyMs, debounceMs: t.debounceMs }); }, [t.latencyMs, t.debounceMs]);
  useEffect(() => { document.documentElement.style.setProperty('--cyan', t.accent); }, [t.accent]);
  useEffect(() => { document.body.style.backgroundImage = t.blueprint ? '' : 'none'; }, [t.blueprint]);
  useEffect(() => { const id = setTimeout(() => api.clearFresh(), 1200); return () => clearTimeout(id); });

  const snap = api.snapshot();

  const statusFor = useCallback((nodeId: string) => {
    const now = performance.now();
    const inflight: Record<string, boolean> = {};
    for (const pk of packetsRef.current) {
      if (now - pk.started < pk.dur) { inflight[pk.fromId] = true; inflight[pk.toId] = true; }
    }
    const node = api.getNodes().find((n: any) => n.id === nodeId);
    return node ? api.statusOf(node, inflight) : { kind: 'solo', label: '—', note: '' };
  }, [api]);

  function addNode(opts: any) { const id = api.addNode(opts); setDialog(false); if (!selected) setSelected(id); }
  function removeNode(id: string) {
    api.removeNode(id);
    if (selected === id) { const left = api.getNodes()[0]; setSelected(left ? left.id : null); }
  }
  function reset() {
    if (!confirm('Reset the whole mesh? All nodes, logs and vault state are cleared.')) return;
    packetsRef.current.length = 0;
    engineRef.current = buildEngine();
    setSelected(null); rerender();
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
                return <NodePanel key={focusNode.id} snap={focusNode} api={api} status={statusFor(focusNode.id)} extraClass="is-focus" onRemove={removeNode} />;
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
                  onFocus={() => { setSelected(n.id); setTweak('layout', 'focus'); }} onRemove={removeNode} />
              ))}
            </div>
          )}
        </>
      )}

      {dialog && <AddNodeDialog snap={snap} onCancel={() => setDialog(false)} onAdd={addNode} />}
      {toast && <div className="toast-wrap"><div className="toast"><span className="accent">⚠ </span>{toast.msg}</div></div>}

      <Settings t={t} set={setTweak} />
    </div>
  );
}
