/* ====================================================================
   settings.tsx · the demo's settings panel — same controls as the design
   Tweaks (latency, debounce, layout, accent, map, grid) but self-contained
   and blueprint-dark (no Claude-Design host protocol).
   ==================================================================== */
import React, { useState } from 'react';

export interface Tweaks {
  latencyMs: number;
  debounceMs: number;
  layout: 'columns' | 'rows' | 'focus';
  accent: string;
  showMap: boolean;
  blueprint: boolean;
}

export const TWEAK_DEFAULTS: Tweaks = {
  latencyMs: 520,
  debounceMs: 850,
  layout: 'columns',
  accent: '#5fb6d4',
  showMap: true,
  blueprint: true,
};

const ACCENTS = ['#5fb6d4', '#74cf9e', '#c9a6ee', '#e6c06a'];

function Slider({ label, value, min, max, step, unit, onChange }: any) {
  return (
    <div className="set-row">
      <div className="set-lbl"><span>{label}</span><span className="v">{value}{unit}</span></div>
      <input type="range" min={min} max={max} step={step} value={value} onChange={(e) => onChange(Number(e.target.value))} />
    </div>
  );
}
function Toggle({ label, value, onChange }: any) {
  return (
    <div className="set-row h">
      <span className="set-lbl" style={{ flex: 1 }}>{label}</span>
      <button className={`set-toggle${value ? ' on' : ''}`} onClick={() => onChange(!value)} role="switch" aria-checked={value}><i /></button>
    </div>
  );
}
function Seg({ label, value, options, onChange }: any) {
  return (
    <div className="set-row">
      <div className="set-lbl"><span>{label}</span></div>
      <div className="seg">
        {options.map((o: string) => (
          <button key={o} className={value === o ? 'on' : ''} onClick={() => onChange(o)}>{o}</button>
        ))}
      </div>
    </div>
  );
}

export function Settings({ t, set }: { t: Tweaks; set: (k: keyof Tweaks, v: any) => void }) {
  const [open, setOpen] = useState(false);
  if (!open) {
    return (
      <button className="btn ghost tiny" style={{ position: 'fixed', right: 16, bottom: 16, zIndex: 60 }}
        onClick={() => setOpen(true)} title="Settings">⚙ Settings</button>
    );
  }
  return (
    <div className="settings">
      <div className="settings-hd">
        <b>Settings</b>
        <button className="icon-btn" onClick={() => setOpen(false)} aria-label="Close">✕</button>
      </div>
      <div className="settings-body">
        <div className="set-sect">Network</div>
        <Slider label="Sync latency" value={t.latencyMs} min={80} max={1600} step={20} unit="ms" onChange={(v: number) => set('latencyMs', v)} />
        <Slider label="Commit debounce" value={t.debounceMs} min={200} max={2500} step={50} unit="ms" onChange={(v: number) => set('debounceMs', v)} />
        <div className="set-sect">Layout</div>
        <Seg label="Arrange" value={t.layout} options={['columns', 'rows', 'focus']} onChange={(v: any) => set('layout', v)} />
        <Toggle label="Network map" value={t.showMap} onChange={(v: boolean) => set('showMap', v)} />
        <div className="set-sect">Appearance</div>
        <div className="set-row">
          <div className="set-lbl"><span>Accent</span></div>
          <div className="swatches">
            {ACCENTS.map((c) => (
              <button key={c} className={`swatch${t.accent === c ? ' on' : ''}`} style={{ background: c, color: c }} onClick={() => set('accent', c)} aria-label={c} />
            ))}
          </div>
        </div>
        <Toggle label="Blueprint grid" value={t.blueprint} onChange={(v: boolean) => set('blueprint', v)} />
      </div>
    </div>
  );
}
