import { describe, expect, it } from '../test-shim';
import { isHidden, prettyName } from './prettyNames';

describe('prettyNames', () => {
  it('detects hidden (dot) files', () => {
    expect(isHidden('.gitignore')).toBe(true);
    expect(isHidden('README.md')).toBe(false);
  });

  it('shows dotfiles verbatim', () => {
    expect(prettyName('.gitignore', false)).toEqual({ label: '.gitignore', italic: false });
  });

  it('titleizes directories', () => {
    expect(prettyName('my-notes_folder', true)).toEqual({ label: 'My Notes Folder', italic: false });
  });

  it('titleizes markdown notes and flags ALL-CAPS stems italic', () => {
    expect(prettyName('quick-thoughts.md', false)).toEqual({ label: 'Quick Thoughts', italic: false });
    expect(prettyName('README.md', false)).toEqual({ label: 'Readme', italic: true });
    expect(prettyName('TODO.md', false)).toEqual({ label: 'Todo', italic: true });
  });

  it('leaves non-markdown files alone', () => {
    expect(prettyName('sync.ts', false)).toEqual({ label: 'sync.ts', italic: false });
  });

  it('handles leading separators (empty title segments)', () => {
    expect(prettyName('-drafts', true)).toEqual({ label: ' Drafts', italic: false });
  });
});
