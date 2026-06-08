// Headless verification of the demo network logic against the REAL wasm engine
// (nodejs glue). Drives the same api the UI uses; asserts real convergence,
// gossip-forward through a hub, and offline → reconnect catch-up.
import { createNetwork } from '../src/engine/network.ts';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failures = 0;
function check(name, cond) {
  console.log(`${cond ? '✓' : '✗ FAIL'}  ${name}`);
  if (!cond) failures++;
}

const api = createNetwork({ latencyMs: 40, debounceMs: 10 });
const filesOf = (id) => {
  const n = api.snapshot().nodes.find((x) => x.id === id);
  const out = {};
  for (const f of Object.values(n.files)) if (!f.deleted) out[f.path] = f.content;
  return out;
};
const nodeByName = (name) => api.snapshot().nodes.find((n) => n.name === name);

// 1) genesis
const laptop = api.addNode({ name: 'laptop' });
check('genesis seeds 5 files', Object.keys(filesOf(laptop)).length === 5);

// 2) clone desktop ← laptop
const desktop = api.addNode({ name: 'desktop', remoteId: laptop });
await sleep(120);
check('desktop cloned laptop (same files)', JSON.stringify(filesOf(desktop)) === JSON.stringify(filesOf(laptop)));

// 3) clone studio ← desktop (chain laptop—desktop—studio)
const studio = api.addNode({ name: 'studio', remoteId: desktop });
await sleep(120);
check('studio cloned desktop', JSON.stringify(filesOf(studio)) === JSON.stringify(filesOf(laptop)));

// 4) edit on laptop → must reach studio FORWARDED through desktop (2 hops)
const readmeId = Object.values(nodeByName('laptop').files).find((f) => f.path === 'README.md').file_id;
api.stageEdit(laptop, readmeId, '# Vault\n\nedited on laptop — propagate through the hub\n');
api.commitNow(laptop, readmeId);
await sleep(250);
check('edit propagated laptop → studio (gossip via hub)',
  filesOf(studio)['README.md'] === '# Vault\n\nedited on laptop — propagate through the hub\n');
check('all three converged on README', filesOf(laptop)['README.md'] === filesOf(studio)['README.md']
  && filesOf(desktop)['README.md'] === filesOf(studio)['README.md']);

// 5) offline studio → edit on laptop → reconnect → version-vector catch-up
api.setOnline(studio, false);
await sleep(20);
const todoId = Object.values(nodeByName('laptop').files).find((f) => f.path === 'notes/todo.md').file_id;
api.stageEdit(laptop, todoId, '# Todo\n\n- [x] offline catch-up works\n');
api.commitNow(laptop, todoId);
await sleep(200);
check('studio MISSED the edit while offline',
  filesOf(studio)['notes/todo.md'] !== '# Todo\n\n- [x] offline catch-up works\n');
const offStatus = api.statusOf(api.getNodes().find((n) => n.id === studio), {});
check('offline studio shows queued/isolated', offStatus.kind === 'offline');
api.setOnline(studio, true);
await sleep(250);
check('reconnect delivered the missed edit (anti-entropy)',
  filesOf(studio)['notes/todo.md'] === '# Todo\n\n- [x] offline catch-up works\n');

// 6) concurrent disjoint edits converge (real 3-way merge)
const ideasL = Object.values(nodeByName('laptop').files).find((f) => f.path === 'notes/ideas.md').file_id;
const ideasS = Object.values(nodeByName('studio').files).find((f) => f.path === 'notes/ideas.md').file_id;
api.stageEdit(laptop, ideasL, 'EDITED-TOP\n- content-addressed blobs\n- lamport ordering\n- rename keeps file_id\n');
api.stageEdit(studio, ideasS, '# Ideas\n\n- content-addressed blobs\n- lamport ordering\n- rename keeps file_id\nEDITED-BOTTOM\n');
api.commitNow(laptop, ideasL);
api.commitNow(studio, ideasS);
await sleep(400);
check('concurrent disjoint edits converge across the mesh',
  filesOf(laptop)['notes/ideas.md'] === filesOf(studio)['notes/ideas.md']
  && filesOf(desktop)['notes/ideas.md'] === filesOf(studio)['notes/ideas.md']);

// final in-sync status
const stat = (id) => api.statusOf(api.getNodes().find((n) => n.id === id), {}).kind;
check('all nodes report in sync', stat(laptop) === 'insync' && stat(desktop) === 'insync' && stat(studio) === 'insync');

console.log(failures === 0 ? '\nALL PASS' : `\n${failures} FAILURE(S)`);
process.exit(failures === 0 ? 0 : 1);
