import { expect, test } from 'bun:test';
import { PathFilter } from '../src/path-filter.ts';

test('private .asp dir is always excluded', () => {
  const f = new PathFilter('');
  expect(f.ignored('.asp')).toBe(true);
  expect(f.ignored('.asp/asp.db')).toBe(true);
  expect(f.ignored('notes/a.md')).toBe(false);
});

test('.aspignore globs + negation', () => {
  const f = new PathFilter('*.log\nbuild/\n!keep.log\n');
  expect(f.ignored('debug.log')).toBe(true);
  expect(f.ignored('keep.log')).toBe(false);
  expect(f.ignored('build/out.js')).toBe(true);
  expect(f.ignored('notes/plan.md')).toBe(false);
});
