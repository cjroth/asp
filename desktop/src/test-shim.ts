// Compatibility shim so the existing vitest-style tests run under `bun test`.
// bun's runtime can't apply vitest's relative-module `vi.mock` (its loader
// bypasses vitest's interceptor), so the suite runs on bun's native test runner
// instead. This maps the `vi.*` surface onto bun's primitives; module mocking
// uses bun's `mock.module` directly in each test (vi.mock → mock.module) so the
// specifier resolves relative to the test file.
import { jest, mock, spyOn } from 'bun:test';

export { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, jest, mock, spyOn, test } from 'bun:test';

const stubbed = new Map<string, { had: boolean; val: unknown }>();

export const vi = {
  fn: (impl?: (...a: unknown[]) => unknown) => mock(impl ?? (() => undefined)),
  spyOn: (obj: object, method: string) => spyOn(obj as never, method as never),
  clearAllMocks: () => jest.clearAllMocks(),
  resetAllMocks: () => jest.restoreAllMocks(),
  restoreAllMocks: () => jest.restoreAllMocks(),
  // bun has no module-registry reset; each test file re-establishes its mocks.
  resetModules: () => {},
  useFakeTimers: () => jest.useFakeTimers(),
  useRealTimers: () => jest.useRealTimers(),
  advanceTimersByTime: (ms: number) => jest.advanceTimersByTime(ms),
  // vitest-only helper bun lacks: advance fake timers, then drain the microtask
  // queue so any async work the fired callbacks kicked off settles.
  advanceTimersByTimeAsync: async (ms: number) => {
    jest.advanceTimersByTime(ms);
    for (let i = 0; i < 12; i++) await Promise.resolve();
  },
  runOnlyPendingTimers: () => (jest as unknown as { runOnlyPendingTimers?: () => void }).runOnlyPendingTimers?.(),
  setSystemTime: (t?: number | Date) => jest.setSystemTime(t as never),
  stubGlobal: (name: string, val: unknown) => {
    if (!stubbed.has(name)) stubbed.set(name, { had: name in globalThis, val: (globalThis as Record<string, unknown>)[name] });
    (globalThis as Record<string, unknown>)[name] = val;
  },
  unstubAllGlobals: () => {
    for (const [name, { had, val }] of stubbed) {
      if (had) (globalThis as Record<string, unknown>)[name] = val;
      else delete (globalThis as Record<string, unknown>)[name];
    }
    stubbed.clear();
  },
  mocked: <T>(x: T) => x as T,
  hoisted: <T>(f: () => T) => f(),
};
