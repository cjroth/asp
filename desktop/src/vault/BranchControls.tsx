// The branch switcher + network-graph launcher — the interactive "branch in the
// UI" surface. A chip shows the checked-out branch; its dropdown lists every
// branch (click to switch), creates a new branch from the current point, deletes
// branches, and opens the GitHub-style network graph. All operations go through
// `api` (→ desktop engine / wasm), so they converge and sync like everything else.

import { useCallback, useEffect, useState } from 'react';
import { api, type BranchGraphData, type BranchInfo } from '../lib/api';
import BranchGraph from './BranchGraph';

export interface BranchControlsProps {
  vaultId: string;
  accent: string;
  /** Re-load the working tree after a checkout/create/delete (the files change). */
  onChanged: () => void;
}

export default function BranchControls({ vaultId, accent, onChanged }: BranchControlsProps) {
  const [branches, setBranches] = useState<BranchInfo[]>([]);
  const [open, setOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [graph, setGraph] = useState<BranchGraphData | null>(null);

  const reload = useCallback(async () => {
    try {
      setBranches(await api.listBranches(vaultId));
    } catch {
      setBranches([]);
    }
  }, [vaultId]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const current = branches.find((b) => b.current);

  const doCheckout = async (branchId: string) => {
    if (busy) return;
    setBusy(true);
    try {
      await api.checkoutBranch(vaultId, branchId);
      await reload();
      onChanged();
      setOpen(false);
      setGraph(null);
    } finally {
      setBusy(false);
    }
  };

  const doCreate = async () => {
    const n = name.trim();
    if (!n || busy) return;
    setBusy(true);
    try {
      const id = await api.createBranch(vaultId, n);
      await api.checkoutBranch(vaultId, id); // create-and-switch, the expected flow
      setName('');
      setCreating(false);
      await reload();
      onChanged();
      setOpen(false);
    } finally {
      setBusy(false);
    }
  };

  const doDelete = async (branchId: string) => {
    if (busy) return;
    setBusy(true);
    try {
      await api.deleteBranch(vaultId, branchId);
      await reload();
      onChanged();
    } finally {
      setBusy(false);
    }
  };

  const openGraph = async () => {
    setOpen(false);
    try {
      setGraph(await api.branchGraph(vaultId, 200));
    } catch {
      setGraph({ nodes: [], branches: [] });
    }
  };

  return (
    <div style={{ position: 'relative', borderBottom: '1px solid var(--line)' }}>
      <div
        data-testid="branch-switcher"
        className="asp-hover-row"
        onClick={() => setOpen((v) => !v)}
        style={{ display: 'flex', alignItems: 'center', gap: 8, height: 34, padding: '0 14px', boxSizing: 'border-box', cursor: 'pointer', fontSize: 12.5 }}
      >
        <BranchIcon />
        <span style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontWeight: 600 }}>
          {current?.name ?? 'main'}
        </span>
        <span style={{ fontSize: 11, color: 'var(--faint2)' }}>{branches.length > 1 ? `${branches.length} branches` : 'branch'}</span>
      </div>

      {open && (
        <>
          <div onClick={() => setOpen(false)} style={{ position: 'fixed', inset: 0, zIndex: 40 }} />
          <div
            data-testid="branch-menu"
            style={{ position: 'absolute', top: 'calc(100% - 2px)', left: 8, right: 8, zIndex: 41, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 12, boxShadow: '0 12px 32px rgba(28,25,23,0.13)', padding: 6, display: 'flex', flexDirection: 'column', gap: 2 }}
          >
            <div style={{ fontSize: 10.5, fontWeight: 600, letterSpacing: '0.06em', textTransform: 'uppercase', color: 'var(--faint2)', padding: '7px 9px 4px' }}>Switch branch</div>
            <div className="asp-scroll" style={{ overflowY: 'auto', maxHeight: 240, display: 'flex', flexDirection: 'column', gap: 2 }}>
              {branches.map((b) => (
                <div key={b.branch_id} className="asp-hover-soft" style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '7px 9px', borderRadius: 8 }}>
                  <span data-testid={`branch-row-${b.name}`} onClick={() => void doCheckout(b.branch_id)} style={{ flex: 1, minWidth: 0, cursor: 'pointer', fontSize: 13, fontWeight: b.current ? 600 : 400, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', color: b.current ? accent : 'var(--text)' }}>
                    {b.current ? '● ' : ''}
                    {b.name}
                  </span>
                  {b.branch_id !== 'main' && (
                    <span
                      data-testid={`branch-delete-${b.name}`}
                      title="Delete branch"
                      onClick={() => void doDelete(b.branch_id)}
                      style={{ cursor: 'pointer', color: 'var(--faint2)', fontSize: 14, lineHeight: 1, padding: '0 2px' }}
                    >
                      ×
                    </span>
                  )}
                </div>
              ))}
            </div>

            <div style={{ height: 1, background: 'var(--line)', margin: '4px 2px' }} />

            {creating ? (
              <div style={{ display: 'flex', gap: 6, padding: '2px 4px' }}>
                <input
                  data-testid="branch-name-input"
                  autoFocus
                  value={name}
                  placeholder="new-branch-name"
                  onChange={(e) => setName(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void doCreate();
                    if (e.key === 'Escape') { setCreating(false); setName(''); }
                  }}
                  style={{ flex: 1, minWidth: 0, fontSize: 13, padding: '6px 8px', borderRadius: 7, border: '1px solid var(--line)', background: 'var(--bg-sub)', color: 'var(--text)' }}
                />
                <button data-testid="branch-create-confirm" onClick={() => void doCreate()} style={{ fontSize: 12, padding: '0 10px', borderRadius: 7, border: 'none', background: accent, color: '#fff', cursor: 'pointer' }}>
                  Create
                </button>
              </div>
            ) : (
              <div data-testid="branch-new" className="asp-hover-soft" onClick={() => setCreating(true)} style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '7px 9px', borderRadius: 8, cursor: 'pointer', fontSize: 13 }}>
                <span style={{ fontSize: 15, lineHeight: 1, color: 'var(--faint)' }}>+</span>
                <span>New branch from here</span>
              </div>
            )}

            <div data-testid="branch-graph-open" className="asp-hover-soft" onClick={() => void openGraph()} style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '7px 9px', borderRadius: 8, cursor: 'pointer', fontSize: 13 }}>
              <NetworkIcon />
              <span>Network graph</span>
            </div>
          </div>
        </>
      )}

      {graph && (
        <>
          <div onClick={() => setGraph(null)} style={{ position: 'fixed', inset: 0, zIndex: 70, background: 'rgba(28,25,23,0.28)' }} />
          <div
            data-testid="branch-graph-modal"
            style={{ position: 'fixed', zIndex: 71, top: '12vh', left: '50%', transform: 'translateX(-50%)', width: 'min(820px, 92vw)', maxHeight: '74vh', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 14, boxShadow: '0 24px 64px rgba(28,25,23,0.22)', display: 'flex', flexDirection: 'column', overflow: 'hidden' }}
          >
            <div style={{ display: 'flex', alignItems: 'center', padding: '12px 16px', borderBottom: '1px solid var(--line)' }}>
              <span style={{ fontSize: 13.5, fontWeight: 600, flex: 1 }}>Branch network</span>
              <span onClick={() => setGraph(null)} style={{ cursor: 'pointer', fontSize: 18, lineHeight: 1, color: 'var(--faint)' }}>×</span>
            </div>
            <div style={{ flex: 1, minHeight: 0, overflow: 'hidden' }}>
              <BranchGraph data={graph} accent={accent} current={current?.branch_id ?? 'main'} onCheckout={(b) => void doCheckout(b)} />
            </div>
          </div>
        </>
      )}
    </div>
  );
}

function BranchIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" style={{ flex: 'none', color: 'var(--faint)' }}>
      <path d="M5 3.5a1.5 1.5 0 1 0-2 1.41V11.1a1.5 1.5 0 1 0 1 0V8.9c.4.26.9.4 1.4.4h2.2a2.5 2.5 0 0 0 2.45-2H11a1.5 1.5 0 1 0 0-1H10.05A2.5 2.5 0 0 0 7.6 4.5H5.4c-.14 0-.27.02-.4.06V4.9A1.5 1.5 0 0 0 5 3.5Z" stroke="currentColor" strokeWidth="1" strokeLinejoin="round" />
    </svg>
  );
}
function NetworkIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" style={{ flex: 'none', color: 'var(--faint)' }}>
      <circle cx="3" cy="8" r="1.6" stroke="currentColor" strokeWidth="1" />
      <circle cx="13" cy="4" r="1.6" stroke="currentColor" strokeWidth="1" />
      <circle cx="13" cy="12" r="1.6" stroke="currentColor" strokeWidth="1" />
      <path d="M4.5 8 11.5 4M4.5 8l7 4" stroke="currentColor" strokeWidth="1" />
    </svg>
  );
}
