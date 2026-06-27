// The app entry: pick the VaultApi backend by environment.
//   • In a Tauri window (window.__TAURI__ present) → TauriVaultApi (full node).
//   • In a plain browser → WebVaultApi (wasm + OPFS thin node).
import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { TauriVaultApi, type VaultApi } from './lib/api';
import { WebVaultApi } from './lib/web-api';

function detectTauri(): boolean {
  const w = window as unknown as { __TAURI__?: unknown; __TAURI_INTERNALS__?: unknown };
  return !!(w.__TAURI__ || w.__TAURI_INTERNALS__);
}

async function makeApi(): Promise<VaultApi> {
  if (detectTauri()) return new TauriVaultApi();
  const web = new WebVaultApi();
  // E2e / advanced: a relay URL via query param or window global, to point the
  // browser thin node at a self-hosted `asp relay` (default: public relays).
  const q = new URLSearchParams(location.search).get('relay');
  const g = (window as unknown as { __ASP_RELAY_URL__?: string }).__ASP_RELAY_URL__;
  web.relayUrl = q ?? g ?? '';
  await web.ensureBooted();
  return web;
}

void makeApi().then((api) => {
  ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
    <React.StrictMode>
      <App api={api} />
    </React.StrictMode>,
  );
});
