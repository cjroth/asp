import { expect, test } from 'bun:test';
import { normalizePeerUrl } from '../src/index.ts';

test('schemeless host defaults to wss://', () => {
  expect(normalizePeerUrl('hub:9000')).toBe('wss://hub:9000');
  expect(normalizePeerUrl('example.com')).toBe('wss://example.com');
  expect(normalizePeerUrl('example.com/sync')).toBe('wss://example.com/sync');
});

test('explicit ws/wss is left untouched', () => {
  expect(normalizePeerUrl('wss://h:9000')).toBe('wss://h:9000');
  expect(normalizePeerUrl('ws://127.0.0.1:8080')).toBe('ws://127.0.0.1:8080');
});

test('http(s) maps to ws(s)', () => {
  expect(normalizePeerUrl('https://h:9000')).toBe('wss://h:9000');
  expect(normalizePeerUrl('http://127.0.0.1:8080')).toBe('ws://127.0.0.1:8080');
});

test('whitespace trimmed; empty stays empty', () => {
  expect(normalizePeerUrl('  hub:9000  ')).toBe('wss://hub:9000');
  expect(normalizePeerUrl('')).toBe('');
  expect(normalizePeerUrl('   ')).toBe('');
});

test('leading slashes are not doubled into the scheme', () => {
  expect(normalizePeerUrl('//h:9000')).toBe('wss://h:9000');
});
