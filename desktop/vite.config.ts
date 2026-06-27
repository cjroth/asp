import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

// Tauri expects a fixed dev port and ignores the Rust backend dirs.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  // mermaid is a heavy, dynamically-imported ESM dep; pre-bundle it so the first
  // ```mermaid block renders without an on-demand re-optimize/reload (which can
  // silently leave the code fallback in place).
  optimizeDeps: { include: ['mermaid'] },
  build: { outDir: 'dist', target: 'es2021' },
});
