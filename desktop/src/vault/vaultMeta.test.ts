import { afterEach, describe, expect, it, vi } from 'vitest';
import { avatarStyle, glyphOf, hash, hueForId, loadVaultMeta, resolveMeta, saveVaultMeta } from './vaultMeta';

afterEach(() => localStorage.clear());

describe('vaultMeta', () => {
  it('hash is deterministic and unsigned', () => {
    expect(hash('abc')).toBe(hash('abc'));
    expect(hash('abc')).toBeGreaterThanOrEqual(0);
    expect(hueForId('vault-x')).toBe(hash('vault-x') % 360);
  });

  it('loads empty map by default and tolerates corruption', () => {
    expect(loadVaultMeta()).toEqual({});
    localStorage.setItem('asp.vaultmeta.v1', '{bad');
    expect(loadVaultMeta()).toEqual({});
  });

  it('round-trips saved metadata', () => {
    const map = { vid: { name: 'Notes', hue: 222, emoji: '📓' } };
    saveVaultMeta(map);
    expect(loadVaultMeta()).toEqual(map);
  });

  it('saveVaultMeta swallows storage errors', () => {
    const spy = vi.spyOn(localStorage, 'setItem').mockImplementation(() => { throw new Error('quota'); });
    expect(() => saveVaultMeta({ vid: { hue: 1 } })).not.toThrow();
    spy.mockRestore();
  });

  it('resolveMeta falls back to defaults', () => {
    const r = resolveMeta({}, 'vid1', 'massive');
    expect(r.name).toBe('massive');
    expect(r.hue).toBe(hueForId('vid1'));
    expect(r.emoji).toBeNull();
  });

  it('resolveMeta overlays saved metadata', () => {
    const r = resolveMeta({ vid1: { name: 'Custom', hue: 32, emoji: '🚀' } }, 'vid1', 'massive');
    expect(r).toEqual({ name: 'Custom', hue: 32, emoji: '🚀' });
  });

  it('glyphOf prefers emoji, else uppercase initial, else dot', () => {
    expect(glyphOf({ emoji: '🚀', name: 'Work' })).toBe('🚀');
    expect(glyphOf({ name: 'work' })).toBe('W');
    expect(glyphOf({ name: '' })).toBe('·');
  });

  it('avatarStyle tints by hue and adapts to emoji vs monogram', () => {
    const mono = avatarStyle({ hue: 222 }, 34, 10);
    expect(mono.background).toBe('hsl(222 44% 94%)');
    expect(mono.fontWeight).toBe(600);
    const emo = avatarStyle({ hue: 222, emoji: '🚀' }, 34, 10);
    expect(emo.fontWeight).toBeUndefined();
    expect(emo.fontSize).toBe(Math.round(34 * 0.54));
  });
});
