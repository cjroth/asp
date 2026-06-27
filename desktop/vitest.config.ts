import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  plugins: [react()],
  test: {
    environment: 'jsdom',
    // A concrete origin so jsdom gives us a working localStorage (the default
    // about:blank opaque origin makes Storage a no-op).
    environmentOptions: { jsdom: { url: 'http://localhost/' } },
    setupFiles: ['src/test-setup.ts'],
    include: ['src/**/*.test.{ts,tsx}'],
    coverage: {
      provider: 'v8',
      include: ['src/vault/**', 'src/App.tsx', 'src/lib/**'],
      // webApi.ts is the browser-only wasm+OPFS backend (no wasm/OPFS in jsdom).
      exclude: ['src/**/*.test.{ts,tsx}', 'src/vault/icons.tsx', 'src/lib/webApi.ts'],
      thresholds: {
        // Aggregate regression floor. It sits just under 100 because the view
        // layer (App + the React components) is functionally exercised end-to-end,
        // yet its last few percent are cosmetic style-ternaries and contentEditable
        // caret DOM-walk edge branches — asserting those individually adds brittle,
        // low-value tests. All PURE logic/util modules are pinned at 100% below.
        lines: 97,
        statements: 97,
        functions: 94,
        branches: 89,
        // Every pure logic/util module + the thin api shim + the fully-covered
        // CustomizeModal are held at 100% individually so they can't regress.
        '**/vault/{emoji,format,history,log,prefs,prettyNames,tree,vaultMeta}.ts': { statements: 100, branches: 100, functions: 100, lines: 100 },
        '**/lib/api.ts': { statements: 100, branches: 100, functions: 100, lines: 100 },
        '**/CustomizeModal.tsx': { statements: 100, branches: 100, functions: 100, lines: 100 },
      },
    },
  },
});
