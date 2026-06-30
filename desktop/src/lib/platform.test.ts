import { afterEach, describe, expect, it } from '../test-shim';
import { isDesktop } from './platform';

const w = window as unknown as Record<string, unknown>;
afterEach(() => {
  delete w.__TAURI__;
  // test-setup sets __TAURI_INTERNALS__ by default; restore it.
  w.__TAURI_INTERNALS__ = {};
});

describe('isDesktop', () => {
  it('is true when Tauri internals are present', () => {
    w.__TAURI_INTERNALS__ = {};
    expect(isDesktop()).toBe(true);
  });

  it('is true when the legacy __TAURI__ global is present', () => {
    delete w.__TAURI_INTERNALS__;
    w.__TAURI__ = {};
    expect(isDesktop()).toBe(true);
  });

  it('is false in a plain browser (no Tauri)', () => {
    delete w.__TAURI_INTERNALS__;
    delete w.__TAURI__;
    expect(isDesktop()).toBe(false);
  });
});
