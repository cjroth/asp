import { describe, expect, it } from '../test-shim';
import { lineDiff } from './diffLines';

describe('lineDiff', () => {
  it('reports identical content as unchanged', () => {
    const d = lineDiff('a\nb\n', 'a\nb\n');
    expect(d.unchanged).toBe(true);
    expect(d.removed).toEqual([]);
    expect(d.added).toEqual([]);
  });

  it('detects an added line, trimming common prefix/suffix', () => {
    const d = lineDiff('a\nb', 'a\nx\nb');
    expect(d.unchanged).toBe(false);
    expect(d.removed).toEqual([]);
    expect(d.added).toEqual(['x']);
    expect(d.atLine).toBe(2);
  });

  it('detects a removed line', () => {
    const d = lineDiff('a\nx\nb', 'a\nb');
    expect(d.removed).toEqual(['x']);
    expect(d.added).toEqual([]);
  });

  it('detects a changed region (removed + added)', () => {
    const d = lineDiff('title\nold body\nfooter', 'title\nnew body\nfooter');
    expect(d.removed).toEqual(['old body']);
    expect(d.added).toEqual(['new body']);
  });

  it('handles creation (empty before) and deletion (empty after)', () => {
    expect(lineDiff('', 'hello\nworld').added).toEqual(['hello', 'world']);
    expect(lineDiff('hello\nworld', '').removed).toEqual(['hello', 'world']);
  });

  it('includes a few context lines before the change', () => {
    const d = lineDiff('l1\nl2\nl3\nold', 'l1\nl2\nl3\nnew', 2);
    expect(d.context).toEqual(['l2', 'l3']);
  });
});
