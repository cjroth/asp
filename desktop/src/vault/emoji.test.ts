import { describe, expect, it } from '../test-shim';
import { EMOJI_CATEGORIES, emojiResults } from './emoji';

describe('emoji', () => {
  it('has well-formed categories', () => {
    expect(EMOJI_CATEGORIES.length).toBeGreaterThan(0);
    for (const c of EMOJI_CATEGORIES) {
      expect(typeof c.name).toBe('string');
      expect(typeof c.icon).toBe('string');
      expect(c.emojis.length).toBeGreaterThan(0);
      for (const [char, kw] of c.emojis) {
        expect(typeof char).toBe('string');
        expect(typeof kw).toBe('string');
      }
    }
  });

  it('returns the active category when there is no query', () => {
    expect(emojiResults('', 0)).toEqual(EMOJI_CATEGORIES[0].emojis.map((p) => p[0]));
    expect(emojiResults('   ', 2)).toEqual(EMOJI_CATEGORIES[2].emojis.map((p) => p[0]));
  });

  it('clamps an out-of-range category index', () => {
    expect(emojiResults('', 999)).toEqual(EMOJI_CATEGORIES[EMOJI_CATEGORIES.length - 1].emojis.map((p) => p[0]));
  });

  it('falls back to the first category for a negative index', () => {
    expect(emojiResults('', -1)).toEqual(EMOJI_CATEGORIES[0].emojis.map((p) => p[0]));
  });

  it('searches across categories by keyword and name', () => {
    expect(emojiResults('rocket', 0)).toContain('🚀');
    expect(emojiResults('dog', 0)).toContain('🐶');
    expect(emojiResults('Smileys', 0)).toContain('😀'); // matches the category name
  });

  it('returns nothing for a non-matching query', () => {
    expect(emojiResults('zzzznotanemoji', 0)).toEqual([]);
  });
});
