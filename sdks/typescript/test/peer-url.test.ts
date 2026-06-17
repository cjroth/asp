import { expect, test } from 'bun:test';
import { normalizePeerUrl } from '../src/index.ts';

// With iroh, a peer is an opaque **ticket** (or a bare node id) — one unbroken
// token, no URL scheme to default or rewrite. normalizePeerUrl strips ALL
// whitespace (not just the ends) so a paste with a stray space or line-wrap
// still connects instead of failing with "bad ticket: invalid symbol at N".
test('peer spec has all whitespace stripped', () => {
  const ticket = 'endpointaaaabbbbccccddddeeeeffff0123456789';
  expect(normalizePeerUrl(`  ${ticket}  `)).toBe(ticket);
  expect(normalizePeerUrl(ticket)).toBe(ticket);
  // Internal whitespace — a wrapped/space-injected paste — is removed too.
  expect(normalizePeerUrl('endpointaaaabbbb  ccccdddd')).toBe('endpointaaaabbbbccccdddd');
  expect(normalizePeerUrl('endpoint\naaaa\tbbbb')).toBe('endpointaaaabbbb');
});

test('empty / whitespace-only stays empty', () => {
  expect(normalizePeerUrl('')).toBe('');
  expect(normalizePeerUrl('   ')).toBe('');
});
