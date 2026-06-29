import { describe, expect, it } from '../test-shim';
import type { HistEvent, VaultStatus } from '../lib/api';
import { deriveLog, logColor, logText, shortFinger } from './log';

const ev = (lamport: number, kind: string, path: string, ts: number): HistEvent => ({ id: 'r' + lamport, ts, lamport, kind, path });
const NOW = 1_700_000_000_000;

const status = (over: Partial<VaultStatus> = {}): VaultStatus => ({
  id: 'v1', vault_id: 'vid1', rows: 12, head: 'h', files: 3,
  listening_ticket: 'asp1abcdefghij', peers: ['ssh-ed25519 PEERKEYMATERIAL a@b'], last_ts: 1, ...over,
});

describe('log', () => {
  it('shortFinger strips the ssh prefix and uppercases', () => {
    expect(shortFinger('ssh-ed25519 AAAAbbbb host')).toBe('AAAABB');
    expect(shortFinger('')).toBe('DEVICE');
  });

  it('logColor maps every level', () => {
    expect(logColor('peer', '#123456')).toBe('#123456');
    expect(logColor('net', '#123456')).toBe('#7c8190');
    expect(logColor('sync', '#123456')).toBe('#2563eb');
    expect(logColor('merge', '#123456')).toBe('#3a9357');
    expect(logColor('vault', '#123456')).toBe('#3a9357');
    expect(logColor('ok', '#123456')).toBe('#3a9357');
    expect(logColor('disk', '#123456')).toBe('#b6612e');
    expect(logColor('warn', '#123456')).toBe('#c0392b');
    expect(logColor('row', '#123456')).toBe('var(--faint)');
  });

  it('derives framing + per-row lines from real events', () => {
    const events = [ev(1, 'create', 'README.md', 1700000000), ev(2, 'edit', 'README.md', 1700000060), ev(3, 'rename', 'a.md', 1700000120)];
    const lines = deriveLog(events, status(), 'ssh-ed25519 DEVICEKEY me@host', { now: NOW });
    const text = logText(lines);
    expect(text).toContain('endpoint bound');
    expect(text).toContain('listening · ticket asp1abcdef… printed');
    expect(text).toContain('dial 1 peer');
    expect(text).toContain('integrate r1 · create README.md');
    expect(text).toContain('create README.md');
    expect(text).toContain('a.md · path moved');
    expect(text).toMatch(/in sync · 1 peer · 12 rows/);
    // raw column format: time + padded level + msg
    expect(lines[0].raw).toMatch(/^\d\d:\d\d:\d\d\.\d\d\d {2}net {4}endpoint/);
  });

  it('handles a private vault with no peers and no events', () => {
    const lines = deriveLog([], status({ peers: [], listening_ticket: null, rows: 0 }), 'ssh-ed25519 K x', { now: NOW });
    const text = logText(lines);
    expect(text).toContain('private · not accepting connections');
    expect(text).toContain('dial 0 peers');
    expect(text).toContain('in sync · 0 peers · 0 rows');
  });

  it('falls back to event count for rows when status is missing', () => {
    const lines = deriveLog([ev(1, 'edit', 'x.md', 1700000000)], undefined, 'ssh-ed25519 K x', { now: NOW });
    expect(logText(lines)).toContain('· 1 rows');
  });

  it('caps the number of per-row lines via maxEvents', () => {
    const events = Array.from({ length: 100 }, (_, i) => ev(i + 1, 'edit', 'n' + i + '.md', 1700000000 + i));
    const lines = deriveLog(events, status(), 'ssh-ed25519 K x', { now: NOW, maxEvents: 5 });
    const integrates = lines.filter((l) => l.msg.startsWith('integrate'));
    expect(integrates.length).toBe(5);
    expect(integrates[integrates.length - 1].msg).toContain('n99.md');
  });
});
