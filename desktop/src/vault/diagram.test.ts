import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  _clearDiagramCache,
  applyCachedDiagrams,
  diagramPreviewHtml,
  fenceInfo,
  isDiagramLang,
  type MermaidLike,
  renderDiagrams,
} from './diagram';

afterEach(() => _clearDiagramCache());

const host = (html: string): HTMLDivElement => {
  const d = document.createElement('div');
  d.innerHTML = html;
  return d;
};

describe('isDiagramLang', () => {
  it('matches mermaid and the diagram alias, case-insensitively', () => {
    expect(isDiagramLang('mermaid')).toBe(true);
    expect(isDiagramLang('Mermaid')).toBe(true);
    expect(isDiagramLang('diagram')).toBe(true);
    expect(isDiagramLang('  mermaid  ')).toBe(true);
    expect(isDiagramLang('mermaid foo')).toBe(true); // trailing attrs still count
  });
  it('rejects other code-fence languages', () => {
    expect(isDiagramLang('js')).toBe(false);
    expect(isDiagramLang('')).toBe(false);
    expect(isDiagramLang('mermaidish')).toBe(false);
    expect(isDiagramLang('diagrammatic')).toBe(false);
  });
});

describe('fenceInfo', () => {
  it('extracts the info string after the opening backticks', () => {
    expect(fenceInfo('```mermaid')).toBe('mermaid');
    expect(fenceInfo('```  diagram ')).toBe('diagram');
    expect(fenceInfo('```')).toBe('');
  });
});

describe('diagramPreviewHtml', () => {
  it('builds a contenteditable=false .md-diagram carrying escaped source + code fallback', () => {
    const d = host(diagramPreviewHtml('graph TD\nA --> B'));
    const node = d.querySelector('.md-diagram') as HTMLElement;
    expect(node).not.toBeNull();
    expect(node.getAttribute('contenteditable')).toBe('false');
    expect(node.getAttribute('data-diagram-src')).toBe('graph TD\nA --> B');
    expect(node.querySelector('.md-diagram-fallback')!.textContent).toBe('graph TD\nA --> B');
  });
  it('escapes markup in both the attribute and the fallback', () => {
    const html = diagramPreviewHtml('a < b & "c"');
    expect(html).toContain('&quot;c&quot;');
    expect(html).toContain('a &lt; b &amp;');
    expect(html).not.toContain('<b');
    // round-trips back to the exact source via the decoded attribute
    expect(host(html).querySelector('.md-diagram')!.getAttribute('data-diagram-src')).toBe('a < b & "c"');
  });
  it('renders a <br> placeholder for an empty diagram body', () => {
    expect(diagramPreviewHtml('')).toContain('<br>');
  });
});

const fakeMermaid = (overrides: Partial<MermaidLike> = {}): MermaidLike => ({
  initialize: vi.fn(),
  render: vi.fn(async (_id: string, src: string) => ({ svg: '<svg data-src="' + src + '"></svg>' })),
  ...overrides,
});

describe('renderDiagrams', () => {
  it('renders each .md-diagram to SVG and marks it rendered', async () => {
    const d = host(diagramPreviewHtml('graph TD\nA-->B'));
    const m = fakeMermaid();
    await renderDiagrams(d, async () => m);
    const node = d.querySelector('.md-diagram') as HTMLElement;
    expect(node.querySelector('svg')).not.toBeNull();
    expect(node.getAttribute('data-diagram-rendered')).toBe('graph TD\nA-->B');
    expect(m.initialize).toHaveBeenCalled();
  });

  it('is a no-op when there are no diagrams (loader never invoked)', async () => {
    const load = vi.fn(async () => fakeMermaid());
    await renderDiagrams(host('<div>plain</div>'), load);
    expect(load).not.toHaveBeenCalled();
  });

  it('keeps the code fallback when the loader rejects (mermaid unavailable)', async () => {
    const d = host(diagramPreviewHtml('graph TD'));
    await renderDiagrams(d, async () => {
      throw new Error('offline');
    });
    const node = d.querySelector('.md-diagram') as HTMLElement;
    expect(node.querySelector('svg')).toBeNull();
    expect(node.querySelector('.md-diagram-fallback')!.textContent).toBe('graph TD');
    expect(node.getAttribute('data-diagram-rendered')).toBeNull();
  });

  it('keeps the code fallback when a single diagram fails to parse', async () => {
    const d = host(diagramPreviewHtml('not a diagram'));
    const m = fakeMermaid({ render: vi.fn(async () => { throw new Error('parse'); }) });
    await renderDiagrams(d, async () => m);
    const node = d.querySelector('.md-diagram') as HTMLElement;
    expect(node.querySelector('svg')).toBeNull();
    expect(node.querySelector('.md-diagram-fallback')).not.toBeNull();
  });

  it('survives an initialize that throws, still rendering', async () => {
    const d = host(diagramPreviewHtml('graph TD'));
    const m = fakeMermaid({ initialize: vi.fn(() => { throw new Error('init'); }) });
    await renderDiagrams(d, async () => m);
    expect((d.querySelector('.md-diagram') as HTMLElement).querySelector('svg')).not.toBeNull();
  });

  it('tolerates mermaid without an initialize method', async () => {
    const d = host(diagramPreviewHtml('graph TD'));
    const m: MermaidLike = { render: vi.fn(async (_id, src) => ({ svg: '<svg>' + src + '</svg>' })) };
    await renderDiagrams(d, async () => m);
    expect((d.querySelector('.md-diagram') as HTMLElement).querySelector('svg')).not.toBeNull();
  });

  it('caches by source: a second render of the same source reuses the SVG (render not re-called)', async () => {
    const m = fakeMermaid();
    const d1 = host(diagramPreviewHtml('graph TD'));
    await renderDiagrams(d1, async () => m);
    expect(m.render).toHaveBeenCalledTimes(1);
    // A fresh placeholder with the same source resolves from cache, not a re-render.
    const d2 = host(diagramPreviewHtml('graph TD'));
    await renderDiagrams(d2, async () => m);
    expect(m.render).toHaveBeenCalledTimes(1);
    expect((d2.querySelector('.md-diagram') as HTMLElement).querySelector('svg')).not.toBeNull();
  });
});

describe('applyCachedDiagrams', () => {
  it('reports all diagrams pending when nothing is cached', () => {
    const d = host(diagramPreviewHtml('a') + diagramPreviewHtml('b'));
    expect(applyCachedDiagrams(d)).toBe(2);
    expect(d.querySelector('svg')).toBeNull();
  });

  it('synchronously replays cached SVGs and reports the remaining pending count', async () => {
    const m = fakeMermaid();
    const warm = host(diagramPreviewHtml('cached'));
    await renderDiagrams(warm, async () => m); // populates the cache for "cached"
    const d = host(diagramPreviewHtml('cached') + diagramPreviewHtml('fresh'));
    expect(applyCachedDiagrams(d)).toBe(1); // only "fresh" is pending
    const nodes = d.querySelectorAll('.md-diagram');
    expect(nodes[0].querySelector('svg')).not.toBeNull();
    expect(nodes[0].getAttribute('data-diagram-rendered')).toBe('cached');
    expect(nodes[1].querySelector('svg')).toBeNull();
  });

  it('treats a .md-diagram with no data-diagram-src as empty source (defensive)', async () => {
    const d = host('<div class="md-diagram"></div>');
    expect(applyCachedDiagrams(d)).toBe(1); // pending: nothing cached for ''
    const m = fakeMermaid();
    await renderDiagrams(d, async () => m);
    const node = d.querySelector('.md-diagram') as HTMLElement;
    expect(m.render).toHaveBeenCalledWith(expect.any(String), '');
    expect(node.getAttribute('data-diagram-rendered')).toBe('');
    // Now cached for '' → a fresh bare node fills synchronously.
    const d2 = host('<div class="md-diagram"></div>');
    expect(applyCachedDiagrams(d2)).toBe(0);
  });

  it('does not re-touch a node already marked rendered for its source', async () => {
    const m = fakeMermaid();
    const d = host(diagramPreviewHtml('x'));
    await renderDiagrams(d, async () => m);
    const node = d.querySelector('.md-diagram') as HTMLElement;
    const before = node.innerHTML;
    expect(applyCachedDiagrams(d)).toBe(0);
    expect(node.innerHTML).toBe(before);
  });
});
