import { open } from '@tauri-apps/plugin-dialog';
import { useEffect, useState } from 'react';
import { api, type VaultInfo } from './lib/api';

export default function App() {
  const [vaults, setVaults] = useState<VaultInfo[]>([]);
  const [identity, setIdentity] = useState('');
  const [cloneTicket, setCloneTicket] = useState('');
  const [authKey, setAuthKey] = useState('');
  const [busy, setBusy] = useState(false);

  const reload = async () => setVaults(await api.listVaults());

  useEffect(() => {
    void reload();
    void api.getIdentity().then(setIdentity);
  }, []);

  const addFolder = async () => {
    const dir = await open({ directory: true });
    if (typeof dir === 'string') {
      setBusy(true);
      try {
        await api.addLocalFolder(dir);
        await reload();
      } finally {
        setBusy(false);
      }
    }
  };

  const cloneRemote = async () => {
    const dest = await open({ directory: true });
    if (typeof dest === 'string' && cloneTicket) {
      setBusy(true);
      try {
        await api.cloneRemote(dest, cloneTicket, authKey || undefined);
        await reload();
      } finally {
        setBusy(false);
      }
    }
  };

  return (
    <main style={{ fontFamily: 'system-ui', padding: 24, maxWidth: 820, margin: '0 auto' }}>
      <h1>Context Desktop</h1>
      <p style={{ color: '#666', fontSize: 13, wordBreak: 'break-all' }}>device key: {identity}</p>

      <section style={{ display: 'flex', gap: 8, margin: '16px 0', alignItems: 'center', flexWrap: 'wrap' }}>
        <button type="button" onClick={addFolder} disabled={busy}>
          Add folder
        </button>
        <input placeholder="paste an iroh ticket" value={cloneTicket} onChange={(e) => setCloneTicket(e.target.value)} />
        <input placeholder="auth key" value={authKey} onChange={(e) => setAuthKey(e.target.value)} />
        <button type="button" onClick={cloneRemote} disabled={busy || !cloneTicket}>
          Clone remote
        </button>
      </section>

      {vaults.map((v) => (
        <VaultRow key={v.id} vault={v} authKey={authKey} onChange={reload} />
      ))}
      {vaults.length === 0 && <p style={{ color: '#999' }}>No folders yet — add one or clone a remote vault.</p>}
    </main>
  );
}

function VaultRow({ vault, authKey, onChange }: { vault: VaultInfo; authKey: string; onChange: () => void }) {
  const [ticket, setTicket] = useState<string | null>(vault.listening_ticket);
  const [rows, setRows] = useState<number | null>(null);

  useEffect(() => {
    void api.getStatus(vault.id).then((s) => setRows(s.rows));
  }, [vault.id]);

  const toggleListen = async () => {
    const t = await api.setAllowConnections(vault.id, ticket == null, authKey || undefined);
    setTicket(t);
    onChange();
  };

  return (
    <div style={{ border: '1px solid #ddd', borderRadius: 8, padding: 12, marginBottom: 10 }}>
      <div style={{ fontWeight: 600 }}>{vault.path}</div>
      <div style={{ fontSize: 12, color: '#777' }}>
        vault {vault.vault_id.slice(0, 8)} · {rows ?? '…'} rows
        {ticket != null ? ` · listening (ticket ${ticket.slice(0, 20)}…)` : ''}
      </div>
      <div style={{ marginTop: 8, display: 'flex', gap: 8 }}>
        <button type="button" onClick={toggleListen}>
          {ticket == null ? "Allow connections" : "Stop listening"}
        </button>
      </div>
    </div>
  );
}
