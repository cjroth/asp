import { expect, test } from 'bun:test';
import { normalizePeerUrl } from '../src/index.ts';

// With iroh, a peer is an opaque **ticket** (or a bare node id) — there is no URL
// scheme to default or rewrite. normalizePeerUrl just trims; the value is passed
// through untouched (kept as a named export for callers that previously
// normalized a URL).
test('peer spec is trimmed and otherwise passed through untouched', () => {
  const ticket = 'endpointaaaabbbbccccddddeeeeffff0123456789';
  expect(normalizePeerUrl(`  ${ticket}  `)).toBe(ticket);
  expect(normalizePeerUrl(ticket)).toBe(ticket);
});

test('empty / whitespace-only stays empty', () => {
  expect(normalizePeerUrl('')).toBe('');
  expect(normalizePeerUrl('   ')).toBe('');
});
