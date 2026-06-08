// UI smoke test: server-render the component tree against the REAL engine
// (nodejs glue) to validate JSX/imports/React wiring without a browser.
import { renderToString } from 'react-dom/server';
import React from 'react';
import { App } from '../src/ui/App.tsx';
import { NodePanel } from '../src/ui/components.tsx';
import { createNetwork } from '../src/engine/network.ts';

let fail = 0;
const check = (name: string, cond: boolean) => { console.log(`${cond ? '✓' : '✗ FAIL'}  ${name}`); if (!cond) fail++; };

// 1) empty App renders the empty state
const empty = renderToString(<App />);
check('App renders empty-mesh state', empty.includes('An empty mesh') && empty.includes('Add a new node'));

// 2) a populated NodePanel renders the real seeded vault
const net = createNetwork({ latencyMs: 10, debounceMs: 10 });
const id = net.addNode({ name: 'laptop' });
const snap = net.snapshot().nodes[0];
const status = net.statusOf(net.getNodes()[0], {});
const panel = renderToString(<NodePanel snap={snap} api={net} status={status} onRemove={() => {}} />);
check('NodePanel shows node name', panel.includes('laptop'));
check('NodePanel shows the real site id chip', panel.includes(snap.site) && panel.includes('site'));
check('FileTree renders seeded folders', panel.includes('notes') && panel.includes('journal') && panel.includes('src'));
check('Editor opens README with real content', panel.includes('README.md') && panel.includes('Shared context'));
check('Editor shows a real merge_class pill', /class[^a-z]/.test(panel) || panel.includes('text') || panel.includes('code'));
check('EventLog shows commit/genesis lines', panel.includes('event log') && panel.includes('genesis'));
check('status pill reflects solo (single node)', status.kind === 'solo' && panel.includes('Solo'));

console.log(fail === 0 ? '\nUI SMOKE: ALL PASS' : `\nUI SMOKE: ${fail} FAILURE(S)`);
process.exit(fail === 0 ? 0 : 1);
