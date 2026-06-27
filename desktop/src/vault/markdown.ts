// Live (contentEditable) Markdown WYSIWYG — a faithful port of the design's
// `renderLiveHtml`/`renderCodeHtml`/`readLive`/caret helpers. Each source line
// becomes one top-level <div> (empty line → <br>), and literal markdown syntax is
// wrapped in hidden `.cm-mark` spans so it round-trips through `readLive` while
// staying invisible. This 1:1 line↔div mapping is what keeps caret math stable.
// All color comes from CSS variables so the editor themes with the rest of the app.
import type { FrontmatterStyle } from './prefs';

const esc = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
const mk = (t: string) => '<span class="cm-mark">' + t + '</span>';

export function inlineMd(raw: string): string {
  let s = esc(raw);
  s = s.replace(/`([^`]+)`/g, (_m, a) => mk('`') + '<code class="cm-code">' + a + '</code>' + mk('`'));
  s = s.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_m, t, u) => mk('[') + '<span class="cm-link">' + t + '</span>' + mk('](' + u + ')'));
  s = s.replace(/\*\*([^*]+)\*\*/g, (_m, a) => mk('**') + '<strong>' + a + '</strong>' + mk('**'));
  s = s.replace(/(^|[^*\w])\*([^*\n]+)\*/g, (_m, p, a) => p + mk('*') + '<em>' + a + '</em>' + mk('*'));
  return s;
}

const divOf = (cls: string, style: string, inner: string) =>
  '<div' + (cls ? ' class="' + cls + '"' : '') + (style ? ' style="' + style + '"' : '') + '>' + (inner === '' ? '<br>' : inner) + '</div>';

// True when a document's rendering is context-dependent line-to-line (a code
// fence, or leading YAML frontmatter) — the LiveEditor must do a FULL re-render
// rather than a single-line one in these cases.
export function hasFrontmatter(src: string): boolean {
  const lines = String(src).replace(/\r/g, '').split('\n');
  if (lines[0] === undefined || lines[0].trim() !== '---') return false;
  for (let j = 1; j < lines.length; j++) if (lines[j].trim() === '---') return true;
  return false;
}

export function renderLiveHtml(src: string, accent = '#3d63dd', fmStyle: FrontmatterStyle = 'Below'): string {
  const lines = String(src).replace(/\r/g, '').split('\n');
  const div = divOf;
  // ---- frontmatter (YAML between leading --- fences) → properties block ----
  let fmEnd = -1;
  if (lines[0] !== undefined && lines[0].trim() === '---') {
    for (let j = 1; j < lines.length; j++) {
      if (lines[j].trim() === '---') {
        fmEnd = j;
        break;
      }
    }
  }
  let html = '';
  let inFence = false;
  for (let i = 0; i < lines.length; i++) {
    const ln = lines[i];
    if (fmEnd > 0 && i <= fmEnd) {
      const fmRow = ln.match(/^(\s*)([\w.$-]+)(:)(\s*)(.*)$/);
      if (fmStyle === 'Banner' || fmStyle === 'Below') {
        const P = fmStyle === 'Banner' ? 'fmb' : 'fmd';
        if (i === 0) {
          html += div(P + '-start', '', mk(esc(ln)));
          continue;
        }
        if (i === fmEnd) {
          html += div(P + '-end', '', mk(esc(ln)));
          continue;
        }
        if (fmRow) {
          const isTitle = /^title$/i.test(fmRow[2]);
          const cls = isTitle ? P + '-line ' + P + '-title' : P + '-line ' + P + '-meta';
          const vcls = /^\[.*\]$/.test(fmRow[5].trim()) ? P + '-val ' + P + '-arr' : P + '-val';
          html += div(cls, '', esc(fmRow[1]) + '<span class="' + P + '-key">' + esc(fmRow[2]) + mk(fmRow[3]) + fmRow[4] + '</span>' + '<span class="' + vcls + '">' + (esc(fmRow[5]) || '<br>') + '</span>');
        } else {
          html += div(P + '-line ' + P + '-meta', '', esc(ln) || '<br>');
        }
        continue;
      }
      // Card (default)
      if (i === 0) {
        html += div('fm-line fm-top', '', mk(esc(ln)));
        continue;
      }
      if (i === fmEnd) {
        html += div('fm-line fm-bot', '', mk(esc(ln)));
        continue;
      }
      if (fmRow) {
        const vcls = /^\[.*\]$/.test(fmRow[5].trim()) ? 'fm-val fm-arr' : 'fm-val';
        html += div('fm-line fm-row', '', esc(fmRow[1]) + '<span class="fm-key">' + esc(fmRow[2]) + mk(fmRow[3]) + fmRow[4] + '</span>' + '<span class="' + vcls + '">' + (esc(fmRow[5]) || '<br>') + '</span>');
      } else {
        html += div('fm-line', '', esc(ln) || '<br>');
      }
      continue;
    }
    if (/^```/.test(ln)) {
      inFence = !inFence;
      html += div('', 'background:var(--bg-input);padding:4px 14px;border-radius:' + (inFence ? '9px 9px 0 0' : '0 0 9px 9px'), mk(esc(ln)));
      continue;
    }
    if (inFence) {
      html += div('', 'font-family:JetBrains Mono,monospace;font-size:13.5px;color:var(--text2);background:var(--bg-input);padding:1px 14px', esc(ln) || '<br>');
      continue;
    }
    if (/^\s*\|.*\|\s*$/.test(ln)) {
      const isSep = /^[\s|:-]+$/.test(ln) && ln.indexOf('-') >= 0;
      const nx = lines[i + 1] || '';
      const isHeader = !isSep && /^\s*\|.*\|\s*$/.test(nx) && /^[\s|:-]+$/.test(nx) && nx.indexOf('-') >= 0;
      const segs = ln.split('|');
      let cells = '<span class="cm-mark">' + esc(segs[0]) + '</span>';
      for (let s = 1; s < segs.length; s++) {
        cells += '<span class="cm-mark">|</span>';
        if (s === segs.length - 1) cells += '<span class="cm-mark">' + esc(segs[s]) + '</span>';
        else if (isSep) cells += '<span class="tcell"><span class="cm-mark">' + esc(segs[s]) + '</span></span>';
        else cells += '<span class="tcell">' + (inlineMd(segs[s]) || '<br>') + '</span>';
      }
      const cls = isSep ? 'tbl-row tbl-sep' : isHeader ? 'tbl-row tbl-head' : 'tbl-row';
      html += '<div class="' + cls + '">' + cells + '</div>';
      continue;
    }
    let m: RegExpMatchArray | null;
    if ((m = ln.match(/^(#{1,4})(\s+)(.*)$/))) {
      const lv = m[1].length;
      const sz = [0, 26, 21, 17.5, 15.5][lv];
      html += div('', 'font-size:' + sz + 'px;font-weight:600;letter-spacing:-0.02em;line-height:1.3;margin:' + (lv === 1 ? '2px' : '18px') + ' 0 4px', mk(m[1] + m[2]) + (inlineMd(m[3]) || '<br>'));
      continue;
    }
    if ((m = ln.match(/^(>\s?)(.*)$/))) {
      html += div('', 'border-left:3px solid ' + accent + '55;padding:1px 0 1px 14px;color:var(--text3);font-style:italic;margin:2px 0', mk(m[1]) + (inlineMd(m[2]) || '<br>'));
      continue;
    }
    if (/^(-{3,}|\*{3,})\s*$/.test(ln)) {
      html += div('', 'border-bottom:1px solid var(--line);line-height:1;padding-bottom:11px;margin:8px 0', mk(esc(ln)));
      continue;
    }
    if ((m = ln.match(/^(\s*)([-*])(\s+)(\[[ xX]\])(\s+)(.*)$/))) {
      const done = /[xX]/.test(m[4]);
      const ind = m[1].length ? 'margin-left:' + m[1].length * 0.55 + 'em' : '';
      html += div('cm-task' + (done ? ' cm-task-done' : ''), ind, mk(m[1] + m[2] + m[3] + m[4] + m[5]) + '<span class="cm-body">' + (inlineMd(m[6]) || '<br>') + '</span>');
      continue;
    }
    if ((m = ln.match(/^(\s*)([-*])(\s+)(.*)$/))) {
      const ind = m[1].length ? 'margin-left:' + m[1].length * 0.55 + 'em' : '';
      html += div('cm-ul', ind, mk(m[1] + m[2] + m[3]) + (inlineMd(m[4]) || '<br>'));
      continue;
    }
    if ((m = ln.match(/^(\s*)(\d+\.)(\s+)(.*)$/))) {
      const ind = m[1].length ? 'margin-left:' + m[1].length * 0.55 + 'em' : '';
      html += div('', 'padding-left:0.2em' + (ind ? ';' + ind : ''), mk(m[1]) + '<span style="color:' + accent + ';font-weight:500">' + esc(m[2]) + '</span>' + m[3] + (inlineMd(m[4]) || '<br>'));
      continue;
    }
    html += div('', 'margin:0;line-height:1.8', inlineMd(ln));
  }
  return html;
}

const CODE_EXT = /\.(ts|tsx|js|jsx|mjs|cjs|json|sh|bash|zsh|css|scss|py|rs|go|toml|ya?ml|html|sql)$/i;
export function isCodeFile(path: string): boolean {
  return CODE_EXT.test(path);
}
export function langOf(path: string): string {
  const m = (path.match(/\.([a-z0-9]+)$/i) || ['', ''])[1].toLowerCase();
  if (['ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs'].includes(m)) return 'js';
  if (m === 'json') return 'json';
  if (['sh', 'bash', 'zsh'].includes(m)) return 'sh';
  if (['yml', 'yaml'].includes(m)) return 'yaml';
  if (['css', 'scss'].includes(m)) return 'css';
  return m || 'txt';
}

// Per-line syntax highlighter for code files (one <div> per source line, so the
// LiveEditor's single-line re-render works). Faithful port of `renderCodeHtml`.
const KEYWORDS: Record<string, string> = {
  js: 'import export from default const let var function return async await class extends new delete typeof instanceof void this super if else for while do switch case break continue try catch finally throw yield in of interface type enum implements public private protected readonly static get set as satisfies',
  sh: 'if then elif else fi for while do done case esac function return export local readonly source echo cd exit set unset trap',
  yaml: 'true false null yes no on off',
  css: 'important inherit initial unset auto none flex grid block inline',
  txt: '',
};
export function renderCodeHtml(src: string, lang: string): string {
  const KW = KEYWORDS[lang] || 'true false null';
  const kw = new Set(KW.split(/\s+/).filter(Boolean));
  const cmt = lang === 'sh' || lang === 'yaml' || lang === 'toml' ? '#' : lang === 'js' || lang === 'css' ? '//' : null;
  const C = { cmt: 'var(--faint)', str: '#3a7d4d', num: '#b6612e', kw: '#8250df', lit: '#b6612e', key: '#2563eb', fn: '#7c5cff' };
  const sp = (c: string, t: string) => '<span style="color:' + c + '">' + t + '</span>';
  const cmtRe = cmt === '#' ? '#[^\\n]*' : cmt === '//' ? '\\/\\/[^\\n]*' : null;
  const reSrc = (cmtRe ? '(' + cmtRe + ')|' : '()') + '("(?:[^"\\\\]|\\\\.)*"|\'(?:[^\'\\\\]|\\\\.)*\'|`(?:[^`\\\\]|\\\\.)*`)|(\\b\\d[\\d._eExXa-fA-F]*\\b)|([A-Za-z_$][\\w$]*)';
  const div = (inner: string) => '<div>' + (inner === '' ? '<br>' : inner) + '</div>';
  const isJsonKeyCtx = lang === 'json';
  return String(src)
    .replace(/\r/g, '')
    .split('\n')
    .map((raw) => {
      const re = new RegExp(reSrc, 'g');
      let out = '';
      let last = 0;
      let m: RegExpExecArray | null;
      while ((m = re.exec(raw)) !== null) {
        out += esc(raw.slice(last, m.index));
        last = re.lastIndex;
        if (m[1]) out += sp(C.cmt, '<span style="font-style:italic">' + esc(m[1]) + '</span>');
        else if (m[2]) {
          const after = raw.slice(re.lastIndex).match(/^\s*:/);
          const isKey = isJsonKeyCtx && after;
          out += sp(isKey ? C.key : C.str, esc(m[2]));
        } else if (m[3]) out += sp(C.num, esc(m[3]));
        else if (m[4]) {
          const w = m[4];
          const after = raw.slice(re.lastIndex);
          if (kw.has(w)) out += sp(C.kw, esc(w));
          else if (/^(true|false|null|undefined|NaN)$/.test(w)) out += sp(C.lit, esc(w));
          else if (lang === 'yaml' && /^\s*:/.test(after) && raw.slice(0, m.index).trim() === '') out += sp(C.key, esc(w));
          else if (/^\s*\(/.test(after)) out += sp(C.fn, esc(w));
          else out += esc(w);
        }
      }
      out += esc(raw.slice(last));
      return div(out);
    })
    .join('');
}

// Render whatever the selected file is: code highlighter for code files, the live
// markdown renderer otherwise.
export function renderDoc(src: string, path: string, accent: string, fmStyle: FrontmatterStyle): string {
  return isCodeFile(path) ? renderCodeHtml(src, langOf(path)) : renderLiveHtml(src, accent, fmStyle);
}

// Reconstruct source markdown from the editor DOM: one source line per child
// node (its textContent, which includes the hidden cm-mark syntax); <br> → "".
export function readLive(el: HTMLElement): string {
  const out: string[] = [];
  el.childNodes.forEach((n) => {
    if (n.nodeType === 3) out.push(n.nodeValue || '');
    else if ((n as HTMLElement).nodeName === 'BR') out.push('');
    else out.push((n as HTMLElement).textContent || '');
  });
  return out.join('\n');
}

// Flatten the current selection to a character offset across the line-divs
// (+1 per line boundary), so a re-render can restore the caret.
export function caretOffset(el: HTMLElement): number | null {
  const sel = getSelection();
  if (!sel || !sel.rangeCount) return null;
  const r = sel.getRangeAt(0);
  let offset = 0;
  const kids = [...el.childNodes];
  for (let i = 0; i < kids.length; i++) {
    const child = kids[i] as HTMLElement;
    if (i > 0) offset += 1;
    if (child === (r.endContainer as Node)) {
      offset += child.nodeType === 3 ? r.endOffset : 0;
      return offset;
    }
    if (child.nodeType !== 3 && child.contains(r.endContainer as Node)) {
      let acc = 0;
      const w = document.createTreeWalker(child, NodeFilter.SHOW_TEXT);
      let n: Node | null;
      while ((n = w.nextNode())) {
        if (n === (r.endContainer as Node)) return offset + acc + r.endOffset;
        acc += (n.nodeValue || '').length;
      }
      return offset + acc;
    }
    offset += child.nodeType === 3 ? (child.nodeValue || '').length : child.nodeName === 'BR' ? 0 : (child.textContent || '').length;
  }
  return offset;
}

export function setCaret(el: HTMLElement, target: number): void {
  let remaining = target;
  const kids = [...el.childNodes];
  for (let i = 0; i < kids.length; i++) {
    const child = kids[i] as HTMLElement;
    if (i > 0) {
      if (remaining === 0) {
        placeInNode(child, 0);
        return;
      }
      remaining -= 1;
    }
    const len = child.nodeType === 3 ? (child.nodeValue || '').length : child.nodeName === 'BR' ? 0 : (child.textContent || '').length;
    if (remaining <= len) {
      placeInNode(child, remaining);
      return;
    }
    remaining -= len;
  }
  const last = kids[kids.length - 1] as HTMLElement | undefined;
  if (last) placeInNode(last, last.nodeType === 3 ? (last.nodeValue || '').length : (last.textContent || '').length);
}

function placeInNode(child: HTMLElement, pos: number): void {
  const sel = getSelection();
  if (!sel) return;
  const range = document.createRange();
  if (child.nodeType === 3) {
    range.setStart(child, Math.min(pos, (child.nodeValue || '').length));
  } else {
    const w = document.createTreeWalker(child, NodeFilter.SHOW_TEXT);
    let n: Node | null;
    let acc = 0;
    let last: Node | null = null;
    let placed = false;
    while ((n = w.nextNode())) {
      last = n;
      if (pos <= acc + (n.nodeValue || '').length) {
        range.setStart(n, pos - acc);
        placed = true;
        break;
      }
      acc += (n.nodeValue || '').length;
    }
    if (!placed) {
      if (last) range.setStart(last, (last.nodeValue || '').length);
      else range.setStart(child, 0);
    }
  }
  range.collapse(true);
  sel.removeAllRanges();
  sel.addRange(range);
}

// Caret char offset within a single element, counting ALL text (including the
// hidden cm-mark markers) — for re-highlighting just one line.
export function textOffsetIn(root: HTMLElement): number | null {
  const sel = getSelection();
  if (!sel || !sel.rangeCount) return null;
  const r = sel.getRangeAt(0);
  if (!root.contains(r.endContainer) && r.endContainer !== root) return null;
  let offset = 0;
  const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let n: Node | null;
  while ((n = w.nextNode())) {
    if (n === r.endContainer) return offset + r.endOffset;
    offset += (n.nodeValue || '').length;
  }
  return offset;
}

export function setTextOffsetIn(root: HTMLElement, target: number): void {
  const sel = getSelection();
  if (!sel) return;
  const range = document.createRange();
  let remaining = target;
  const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let n: Node | null;
  let last: Node | null = null;
  while ((n = w.nextNode())) {
    last = n;
    const len = (n.nodeValue || '').length;
    if (remaining <= len) {
      range.setStart(n, remaining);
      range.collapse(true);
      sel.removeAllRanges();
      sel.addRange(range);
      return;
    }
    remaining -= len;
  }
  if (last) range.setStart(last, (last.nodeValue || '').length);
  else range.setStart(root, 0);
  range.collapse(true);
  sel.removeAllRanges();
  sel.addRange(range);
}

export function wordCountOf(content: string): string {
  const words = content.trim() ? content.trim().split(/\s+/).length : 0;
  return words + (words === 1 ? ' word' : ' words');
}

// Status-bar count: words for markdown, lines for everything else (design 1963).
export function countLabel(content: string, path: string): string {
  if (/\.md$/i.test(path)) return wordCountOf(content);
  const n = content.length ? content.replace(/\n+$/, '').split('\n').length : 0;
  return n + (n === 1 ? ' line' : ' lines');
}
