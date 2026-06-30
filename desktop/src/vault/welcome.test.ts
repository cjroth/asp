// Guards the welcome README showcase: every rich feature the editor renders must
// stay present in WELCOME_MD, so the seeded note can't silently lose a demo of a
// feature over time.
import { describe, expect, it } from '../test-shim';
import { WELCOME_MD } from './welcome';

describe('WELCOME_MD', () => {
  it('opens with YAML frontmatter', () => {
    expect(WELCOME_MD.startsWith('---\n')).toBe(true);
    expect(WELCOME_MD).toMatch(/^---\n[\s\S]*?\ntags: \[welcome, guide\][\s\S]*?\n---/);
  });

  it('includes a mermaid diagram', () => {
    expect(WELCOME_MD).toContain('```mermaid');
    expect(WELCOME_MD).toContain('flowchart');
  });

  it('includes a table', () => {
    // A header row, a separator row, and at least one body row.
    expect(WELCOME_MD).toMatch(/\| --- \|/);
    expect(WELCOME_MD).toMatch(/\| Action \| How \|/);
  });

  it('includes checked and unchecked task-list items', () => {
    expect(WELCOME_MD).toContain('- [x]');
    expect(WELCOME_MD).toContain('- [ ]');
  });

  it('includes a blockquote', () => {
    expect(WELCOME_MD).toMatch(/^> /m);
  });

  it('includes fenced code blocks with language tags', () => {
    expect(WELCOME_MD).toContain('```tsx');
    expect(WELCOME_MD).toContain('```bash');
  });

  it('covers the key app concepts', () => {
    for (const topic of [
      'autosave',
      'Sharing',
      'History',
      'Log',
      'Multi-select',
      'folder path',
    ]) {
      expect(WELCOME_MD).toContain(topic);
    }
  });
});
