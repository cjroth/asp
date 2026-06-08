/* ====================================================================
   asp-app.jsx · App root — wires the engine to the UI + Tweaks
   ==================================================================== */
const { useState: useS, useRef: useR, useEffect: useE, useCallback: useC } = React;

const TWEAK_DEFAULTS = /*EDITMODE-BEGIN*/{
  "latencyMs": 520,
  "debounceMs": 850,
  "layout": "columns",
  "accent": "#5fb6d4",
  "showMap": true,
  "blueprint": true
}/*EDITMODE-END*/;

function App() {
  const [t, setTweak] = useTweaks(TWEAK_DEFAULTS);
  const engineRef = useR(null);
  const packetsRef = useR([]);
  const [, setTick] = useS(0);
  const rerender = useC(() => setTick((x) => x + 1), []);
  const [dialog, setDialog] = useS(false);
  const [selected, setSelected] = useS(null);
  const [toast, setToast] = useS(null);
  const toastTimer = useR(null);

  // build engine once
  if (!engineRef.current) {
    engineRef.current = window.ASPEngine.createNetwork({
      latencyMs: TWEAK_DEFAULTS.latencyMs,
      debounceMs: TWEAK_DEFAULTS.debounceMs,
      onChange: () => rerender(),
      onPacket: (pk) => { packetsRef.current.push(pk); },
      onToast: (msg, kind) => {
        setToast({ msg, kind });
        if (toastTimer.current) clearTimeout(toastTimer.current);
        toastTimer.current = setTimeout(() => setToast(null), 2600);
      },
    });
  }
  const api = engineRef.current;

  // push tweaks → engine + theme
  useE(() => { api.setConfig({ latencyMs: t.latencyMs, debounceMs: t.debounceMs }); }, [t.latencyMs, t.debounceMs]);
  useE(() => { document.documentElement.style.setProperty("--cyan", t.accent); }, [t.accent]);
  useE(() => { document.body.style.backgroundImage = t.blueprint ? "" : "none"; }, [t.blueprint]);

  // clear "fresh" flag on log lines shortly after render
  useE(() => { const id = setTimeout(() => { api.clearFresh(); }, 1200); return () => clearTimeout(id); });

  const snap = api.snapshot();

  // inflight + status
  const statusFor = useC((nodeId) => {
    const now = performance.now();
    const inflight = {};
    for (const pk of packetsRef.current) {
      if (now - pk.started < pk.dur) { inflight[pk.fromId] = true; inflight[pk.toId] = true; }
    }
    const node = api.getNodes().find((n) => n.id === nodeId);
    return node ? api.statusOf(node, inflight) : { kind: "solo", label: "—", note: "" };
  }, [api]);

  function addNode(opts) { const id = api.addNode(opts); setDialog(false); if (!selected) setSelected(id); }
  function removeNode(id) {
    api.removeNode(id);
    if (selected === id) { const left = api.getNodes()[0]; setSelected(left ? left.id : null); }
  }
  function reset() {
    if (!confirm("Reset the whole mesh? All nodes, logs and vault state are cleared.")) return;
    packetsRef.current.length = 0;
    engineRef.current = window.ASPEngine.createNetwork({
      latencyMs: t.latencyMs, debounceMs: t.debounceMs,
      onChange: () => rerender(), onPacket: (pk) => packetsRef.current.push(pk),
      onToast: (msg, kind) => { setToast({ msg, kind }); if (toastTimer.current) clearTimeout(toastTimer.current); toastTimer.current = setTimeout(() => setToast(null), 2600); },
    });
    setSelected(null); rerender();
  }

  const nodes = snap.nodes;
  const totalRows = nodes.reduce((a, n) => a + n.rowCount, 0);
  if (selected && !nodes.find((n) => n.id === selected)) { /* stale */ }
  const sel = selected && nodes.find((n) => n.id === selected) ? selected : (nodes[0] && nodes[0].id);

  const layoutClass = t.layout === "focus" ? "layout-focus" : t.layout === "rows" ? "layout-rows" : "cols-" + Math.min(nodes.length, 3);

  return (
    <div className="app">
      {/* top bar */}
      <div className="topbar">
        <div className="brand">
          <span className="mark"><span className="tick"></span>ASP</span>
          <span className="sub">Agent Sync Protocol · p2p sync demo</span>
        </div>
        <div className="spacer"></div>
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

      {/* body */}
      {nodes.length === 0 ? (
        <EmptyState onAdd={() => setDialog(true)} />
      ) : (
        <>
          {t.showMap && <NetworkMap snap={snap} packetsRef={packetsRef} selected={sel} onSelect={setSelected} statusFor={statusFor} />}
          {t.layout === "focus" ? (
            <div className="canvas layout-focus">
              {(() => {
                const focusNode = nodes.find((n) => n.id === sel) || nodes[0];
                return <NodePanel key={focusNode.id} snap={focusNode} api={api} status={statusFor(focusNode.id)} extraClass="is-focus" onRemove={removeNode} />;
              })()}
              <div className="focus-rail">
                {nodes.map((n) => (
                  <NodeStrip key={n.id} snap={n} status={statusFor(n.id)} selected={n.id === sel} onClick={() => setSelected(n.id)} />
                ))}
              </div>
            </div>
          ) : (
            <div className={"canvas " + layoutClass}
              style={t.layout === "columns" ? { gridTemplateColumns: `repeat(${Math.min(nodes.length, 3)}, minmax(0,1fr))` } : undefined}>
              {nodes.map((n) => (
                <NodePanel key={n.id} snap={n} api={api} status={statusFor(n.id)}
                  onFocus={() => { setSelected(n.id); setTweak("layout", "focus"); }} onRemove={removeNode} />
              ))}
            </div>
          )}
        </>
      )}

      {dialog && <AddNodeDialog snap={snap} onCancel={() => setDialog(false)} onAdd={addNode} />}
      {toast && <div className="toast-wrap"><div className="toast"><span className="accent">⚠ </span>{toast.msg}</div></div>}

      {/* Tweaks */}
      <TweaksPanel>
        <TweakSection label="Network" />
        <TweakSlider label="Sync latency" value={t.latencyMs} min={80} max={1600} step={20} unit="ms"
          onChange={(v) => setTweak("latencyMs", v)} />
        <TweakSlider label="Commit debounce" value={t.debounceMs} min={200} max={2500} step={50} unit="ms"
          onChange={(v) => setTweak("debounceMs", v)} />
        <TweakSection label="Layout" />
        <TweakRadio label="Arrange" value={t.layout} options={["columns", "rows", "focus"]}
          onChange={(v) => setTweak("layout", v)} />
        <TweakToggle label="Network map" value={t.showMap} onChange={(v) => setTweak("showMap", v)} />
        <TweakSection label="Appearance" />
        <TweakColor label="Accent" value={t.accent} options={["#5fb6d4", "#74cf9e", "#c9a6ee", "#e6c06a"]}
          onChange={(v) => setTweak("accent", v)} />
        <TweakToggle label="Blueprint grid" value={t.blueprint} onChange={(v) => setTweak("blueprint", v)} />
      </TweaksPanel>
    </div>
  );
}

function EmptyState({ onAdd }) {
  return (
    <div className="empty">
      <div className="empty-block">
        <span className="corner-bl"></span><span className="corner-br"></span>
        <div className="eyebrow">Agent Sync Protocol</div>
        <h1>An empty mesh.<br />Spin up your first node.</h1>
        <p>
          Each node is a device running ASP — a tiny vault of files, an append-only event log,
          and a live peer-to-peer sync engine. Add a node to create a vault; add more and clone
          them from a peer to watch edits propagate, deterministically, with no commit and no push.
        </p>
        <button className="btn primary" onClick={onAdd} style={{ fontSize: 13.5, padding: "10px 18px" }}>
          <span className="glyph">+</span>Add a new node
        </button>
        <div className="legend">
          <span><span className="dot" style={{ background: "var(--green)" }}></span>in sync</span>
          <span><span className="dot" style={{ background: "var(--cyan)" }}></span>syncing</span>
          <span><span className="dot" style={{ background: "var(--red)" }}></span>offline</span>
          <span><span className="dot" style={{ background: "var(--amber)" }}></span>catch-up frame</span>
        </div>
      </div>
    </div>
  );
}
window.EmptyState = EmptyState;

ReactDOM.createRoot(document.getElementById("root")).render(<App />);
