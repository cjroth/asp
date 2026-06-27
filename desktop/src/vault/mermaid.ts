// Thin, browser-only boundary for the mermaid library. Mermaid needs a real DOM
// (no jsdom support) and is heavy, so it is dynamically imported on first use and
// only when a diagram is actually present. The rest of the diagram code depends
// on the injectable `MermaidLoader` type, so this file is the single place that
// names the real module — and the single file excluded from coverage (mirroring
// `src/lib/webApi.ts`).
import type { MermaidLike, MermaidLoader } from './diagram';

export const loadMermaid: MermaidLoader = async (): Promise<MermaidLike> => {
  const mod = (await import('mermaid')) as { default: MermaidLike };
  return mod.default;
};
