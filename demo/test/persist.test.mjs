// OPFS persistence logic: serialize a live mesh, restore it into a fresh
// network, and assert files + topology survive and sync still works. (Pure
// engine logic — the OPFS I/O itself is exercised by the Playwright e2e.)
import { createNetwork } from '../src/engine/network.ts';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let fail = 0;
const check = (n, c) => { console.log(`${c ? '✓' : '✗ FAIL'}  ${n}`); if (!c) fail++; };
const filesOf = (api, id) => {
  const n = api.snapshot().nodes.find((x) => x.id === id);
  const out = {};
  for (const f of Object.values(n.files)) if (!f.deleted) out[f.path] = api.fileText(id, f.path);
  return out;
};

const api = createNetwork({ latencyMs: 30, debounceMs: 10 });
const a = api.addNode({ name: 'laptop' });
const b = api.addNode({ name: 'desktop', remoteId: a });
await sleep(120);
// author an edit on laptop
const readme = Object.values(api.snapshot().nodes[0].files).find((f) => f.path === 'README.md');
api.stageEdit(a, readme.file_id, '# Vault\n\npersisted edit\n');
api.commitNow(a, readme.file_id);
await sleep(120);

const state = api.serialize();
check('serialize captured 2 nodes', state.nodes.length === 2);
check('serialize captured the edge by localId', state.edges.length === 1 && state.edges[0].a && state.edges[0].b);
check('serialize captured rows', state.nodes[0].rows.length > 0);
check('serialize captured names', state.nodes.map((n) => n.name).sort().join() === 'desktop,laptop');

// restore into a fresh network
const api2 = createNetwork({ latencyMs: 30, debounceMs: 10 });
api2.restore(state);
const snap2 = api2.snapshot();
const a2 = snap2.nodes.find((n) => n.name === 'laptop').id;
const b2 = snap2.nodes.find((n) => n.name === 'desktop').id;
check('restored 2 nodes', snap2.nodes.length === 2);
check('restored edge/topology', snap2.edges.length === 1);
check('restored laptop content (incl. the edit)', filesOf(api2, a2)['README.md'] === '# Vault\n\npersisted edit\n');
check('restored desktop converged content', JSON.stringify(filesOf(api2, a2)) === JSON.stringify(filesOf(api2, b2)));

// editing still propagates after restore
const ideas = Object.values(snap2.nodes.find((n) => n.id === a2).files).find((f) => f.path === 'notes/ideas.md');
api2.stageEdit(a2, ideas.file_id, '# Ideas\n\npost-restore edit\n');
api2.commitNow(a2, ideas.file_id);
await sleep(150);
check('post-restore edit propagates across the restored mesh',
  filesOf(api2, b2)['notes/ideas.md'] === '# Ideas\n\npost-restore edit\n');

console.log(fail === 0 ? '\nPERSIST: ALL PASS' : `\nPERSIST: ${fail} FAILURE(S)`);
process.exit(fail === 0 ? 0 : 1);
