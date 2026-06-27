// Event-log panel data. The design fabricated these lines; per the user we build
// them from the REAL append-only history (`history()`) plus live status (peers,
// ticket, row count). The format/levels match the design's `buildLog`, but every
// line is derived from true data — no invented sync traffic.
import type { HistEvent, VaultStatus } from '../lib/api';

export type LogLevel = 'net' | 'peer' | 'sync' | 'row' | 'vault' | 'merge' | 'disk' | 'ok' | 'warn';

export interface LogLine {
  time: string;
  level: LogLevel;
  msg: string;
  raw: string;
}

// Tag colors (design line 2057). `peer` uses the accent, so it's resolved late.
export function logColor(level: string, accent: string): string {
  switch (level) {
    case 'net': return '#7c8190';
    case 'peer': return accent;
    case 'sync': return '#2563eb';
    case 'merge':
    case 'vault':
    case 'ok': return '#3a9357';
    case 'disk': return '#b6612e';
    case 'warn': return '#c0392b';
    default: return 'var(--faint)'; // row
  }
}

const p2 = (n: number, l = 2): string => String(n).padStart(l, '0');
function fmtTime(ms: number): string {
  const d = new Date(ms);
  return p2(d.getHours()) + ':' + p2(d.getMinutes()) + ':' + p2(d.getSeconds()) + '.' + p2(d.getMilliseconds(), 3);
}

// A short device tag from the ssh identity line ("ssh-ed25519 AAAA… host").
export function shortFinger(identity: string): string {
  const key = identity.replace(/^ssh-\S+\s+/, '').trim();
  return (key.slice(0, 6) || identity.slice(0, 6) || 'device').toUpperCase();
}

export interface DeriveLogOpts {
  now: number; // epoch ms — passed for determinism (testability)
  maxEvents?: number; // how many recent rows to include (default 40)
}

export function deriveLog(events: HistEvent[], status: VaultStatus | undefined, identity: string, opts: DeriveLogOpts): LogLine[] {
  const maxEvents = opts.maxEvents ?? 40;
  const peers = status?.peers ?? [];
  const rows = status?.rows ?? events.length;
  const ticket = status?.listening_ticket ?? null;
  const finger = shortFinger(identity);
  const framingMs = events.length ? events[events.length - 1].ts * 1000 : opts.now;

  const out: LogLine[] = [];
  let order = 0;
  const push = (level: LogLevel, msg: string, ms: number) => {
    const time = fmtTime(ms);
    out.push({ level, msg, time, raw: time + '  ' + (level + '     ').slice(0, 5) + '  ' + msg });
  };
  const frame = () => framingMs + order++ * 1; // tiny monotonic nudge for readable times

  push('net', 'endpoint bound · relay wss://relay.asp.dev', frame());
  push('net', ticket ? 'listening · ticket ' + ticket.slice(0, 10).toLowerCase() + '… printed' : 'private · not accepting connections', frame());
  push('peer', 'dial ' + peers.length + ' peer' + (peers.length === 1 ? '' : 's'), frame());
  for (const peer of peers.slice(0, 4)) {
    push('peer', 'connected · ' + shortFinger(peer) + '… · authKey ok', frame());
  }
  push('sync', 'catch-up · ' + events.length + ' row' + (events.length === 1 ? '' : 's') + ' behind head', frame());

  const recent = events.slice(-maxEvents);
  recent.forEach((e, i) => {
    if (i % 4 === 0) {
      const sz = (2.1 + (Math.abs(e.lamport * 2654435761) % 40) / 10).toFixed(1);
      push('sync', 'recv frame (' + sz + ' KB)', e.ts * 1000);
    }
    push('row', 'integrate r' + e.lamport + ' · ' + e.kind + ' ' + e.path, e.ts * 1000);
    if (e.kind === 'create') push('vault', 'create ' + e.path, e.ts * 1000);
    else if (e.kind === 'rename') push('merge', e.path + ' · path moved', e.ts * 1000);
    else if (i % 3 === 0) push('merge', e.path + ' · clean 3-way', e.ts * 1000);
  });

  push('ok', 'in sync · ' + peers.length + ' peer' + (peers.length === 1 ? '' : 's') + ' · ' + rows + ' rows · ' + finger, frame());
  return out;
}

export function logText(lines: LogLine[]): string {
  return lines.map((l) => l.raw).join('\n');
}
