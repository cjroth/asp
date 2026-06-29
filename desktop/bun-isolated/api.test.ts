import { mock } from 'bun:test';
import { afterEach, beforeEach, describe, expect, it, vi } from '../src/test-shim';

const invoke = vi.fn(async () => 'ok');
mock.module('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...(a as [])) }));
mock.module('../src/lib/webApi', () => ({
  createWebApi: () => ({ listVaults: async () => [{ id: 'w1', path: '', vault_id: 'wv', enabled: true, listening_ticket: null }] }),
}));

import { api } from '../src/lib/api';

const w = window as unknown as Record<string, unknown>;
beforeEach(() => invoke.mockClear());
afterEach(() => { w.__TAURI_INTERNALS__ = {}; }); // restore the default desktop platform

describe('api — thin Tauri command surface', () => {
  it('maps each method to its command + params', async () => {
    await api.listVaults();
    expect(invoke).toHaveBeenCalledWith('list_vaults');

    await api.addLocalFolder('/p');
    expect(invoke).toHaveBeenCalledWith('add_local_folder', { path: '/p' });

    await api.cloneRemote('/d', 'tkt', 'key');
    expect(invoke).toHaveBeenCalledWith('clone_remote', { dest: '/d', ticket: 'tkt', authKey: 'key' });

    await api.setAllowConnections('id', true, 'k');
    expect(invoke).toHaveBeenCalledWith('set_allow_connections', { id: 'id', on: true, authKey: 'k' });

    await api.syncNow('id', 'tkt');
    expect(invoke).toHaveBeenCalledWith('sync_now', { id: 'id', ticket: 'tkt', authKey: undefined });

    await api.getStatus('id');
    expect(invoke).toHaveBeenCalledWith('get_status', { id: 'id' });

    await api.getIdentity();
    expect(invoke).toHaveBeenCalledWith('get_identity');

    await api.authorize('id', 'pk');
    expect(invoke).toHaveBeenCalledWith('authorize', { id: 'id', pubkey: 'pk' });

    await api.createSnapshot('id', 'snap');
    expect(invoke).toHaveBeenCalledWith('create_snapshot', { id: 'id', name: 'snap' });

    await api.restore('id', 'tgt');
    expect(invoke).toHaveBeenCalledWith('restore', { id: 'id', target: 'tgt' });

    await api.listFiles('id');
    expect(invoke).toHaveBeenCalledWith('list_files', { id: 'id' });

    await api.readFile('id', 'p');
    expect(invoke).toHaveBeenCalledWith('read_file', { id: 'id', path: 'p' });

    await api.writeFile('id', 'p', 'c');
    expect(invoke).toHaveBeenCalledWith('write_file', { id: 'id', path: 'p', content: 'c' });

    await api.renameFile('id', 'o', 'n');
    expect(invoke).toHaveBeenCalledWith('rename_file', { id: 'id', old: 'o', new: 'n' });

    await api.createDir('id', 'd');
    expect(invoke).toHaveBeenCalledWith('create_dir', { id: 'id', path: 'd' });

    await api.deleteFile('id', 'p');
    expect(invoke).toHaveBeenCalledWith('delete_file', { id: 'id', path: 'p' });

    await api.history('id');
    expect(invoke).toHaveBeenCalledWith('history', { id: 'id' });

    await api.readFileAt('id', 'p', 5);
    expect(invoke).toHaveBeenCalledWith('read_file_at', { id: 'id', path: 'p', ts: 5 });

    await api.restoreFileAt('id', 'p', 5);
    expect(invoke).toHaveBeenCalledWith('restore_file_at', { id: 'id', path: 'p', ts: 5 });

    await api.rescan('id');
    expect(invoke).toHaveBeenCalledWith('rescan', { id: 'id' });

    await api.removeVault('id', true);
    expect(invoke).toHaveBeenCalledWith('remove_vault', { id: 'id', trash: true });

    await api.revealPath('/p');
    expect(invoke).toHaveBeenCalledWith('reveal_path', { path: '/p' });
  });

  it('createVault is rejected on desktop (web-only)', async () => {
    await expect(api.createVault('x')).rejects.toThrow('web-only');
  });

  it('dispatches to the web backend when Tauri is absent', async () => {
    delete w.__TAURI_INTERNALS__;
    const vs = await api.listVaults();
    expect(vs[0].id).toBe('w1');
    expect(invoke).not.toHaveBeenCalled();
  });
});
import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
