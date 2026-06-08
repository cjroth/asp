import { expect, test } from 'bun:test';
import { LogBuffer } from '../src/log-buffer.ts';

test('append + snapshot preserves order and level', () => {
  const log = new LogBuffer();
  log.append('first');
  log.append('boom', 'error');
  const snap = log.snapshot();
  expect(snap.map((e) => e.msg)).toEqual(['first', 'boom']);
  expect(snap[1].level).toBe('error');
  expect(snap[0].ts).toMatch(/^\d\d:\d\d:\d\d\.\d\d\d$/);
});

test('ring buffer evicts oldest beyond capacity', () => {
  const log = new LogBuffer(3);
  for (const m of ['a', 'b', 'c', 'd']) log.append(m);
  expect(log.snapshot().map((e) => e.msg)).toEqual(['b', 'c', 'd']);
});

test('subscribe streams future appends; unsubscribe stops', () => {
  const log = new LogBuffer();
  const seen: string[] = [];
  const unsub = log.subscribe((e) => seen.push(e.msg));
  log.append('one');
  unsub();
  log.append('two');
  expect(seen).toEqual(['one']);
});

test('toText renders copyable lines; clear empties', () => {
  const log = new LogBuffer();
  log.append('hello');
  log.append('bad', 'error');
  const text = log.toText();
  expect(text).toContain('hello');
  expect(text).toContain('ERROR bad');
  log.clear();
  expect(log.snapshot()).toHaveLength(0);
});
