// Live (contentEditable) Markdown WYSIWYG — a faithful port of the design's
// `renderLiveHtml`/`renderCodeHtml`/`readLive`/caret helpers. Each source line
// becomes one top-level <div> (empty line → <br>), and literal markdown syntax is
// wrapped in hidden `.cm-mark` spans so it round-trips through `readLive` while
// staying invisible. This 1:1 line↔div mapping is what keeps caret math stable.
// All color comes from CSS variables so the editor themes with the rest of the app.
import { diagramPreviewHtml, fenceInfo, isDiagramLang } from './diagram';
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
  // While inside a ```mermaid / ```diagram fence we collect the raw source lines
  // so that, on the closing fence, we can append a single rendered `.md-diagram`
  // preview AFTER the fence. The fence lines themselves still render as ordinary
  // editable code-fence divs, so the source round-trips byte-for-byte.
  let inDiagram = false;
  let diagramSrc: string[] = [];
  // When a fence opens with a recognized language (```tsx, ```python, …) we hold
  // its per-line syntax highlighter here and apply it to each body line; null for
  // plain/unknown fences (which stay un-highlighted).
  let fenceHi: ((raw: string) => string) | null = null;
  // A run of consecutive table-row lines is grouped under one `.tbl-scroll`
  // region wrapping an inner `.tbl-grid` (a CSS `display:table` box) so columns
  // ALIGN across rows while only the table scrolls horizontally — without
  // squashing cells or widening the prose. A trailing `.tbl-pad` spacer (a
  // sibling of the grid, NOT a row) gives a content-width table some extra
  // scroll room on the right. `readLive`/caret helpers descend through BOTH
  // wrappers (and skip the spacer) so each `.tbl-row` still maps 1:1 to a
  // source line.
  let inTable = false;
  const closeTable = () => {
    if (inTable) {
      html += '</div><div class="tbl-pad" contenteditable="false" aria-hidden="true"></div></div>';
      inTable = false;
    }
  };
  for (let i = 0; i < lines.length; i++) {
    const ln = lines[i];
    // Close an open table wrapper before emitting any non-table line. A line is
    // a table row only outside frontmatter/code-fences and matching the pipe row.
    const isTableLine = !(fmEnd > 0 && i <= fmEnd) && !inFence && !/^```/.test(ln) && /^\s*\|.*\|\s*$/.test(ln);
    if (inTable && !isTableLine) closeTable();
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
      const opening = !inFence;
      inFence = !inFence;
      html += div('', 'background:var(--bg-input);padding:4px 14px;border-radius:' + (inFence ? '9px 9px 0 0' : '0 0 9px 9px'), mk(esc(ln)));
      if (opening) {
        const info = fenceInfo(ln);
        if (isDiagramLang(info)) {
          inDiagram = true;
          diagramSrc = [];
        } else {
          const key = fenceLang(info);
          fenceHi = key === 'txt' ? null : lineHighlighterFor(key);
        }
      } else {
        if (inDiagram) {
          // Closing a diagram fence: append the (skipped) rendered preview.
          html += diagramPreviewHtml(diagramSrc.join('\n'));
          inDiagram = false;
        }
        fenceHi = null;
      }
      continue;
    }
    if (inFence) {
      if (inDiagram) diagramSrc.push(ln);
      // Highlight recognized-language bodies; plain fences and diagram source
      // stay literal. The highlighter preserves textContent, so readLive still
      // round-trips the source byte-for-byte.
      const inner = !inDiagram && fenceHi ? fenceHi(ln) : esc(ln);
      html += div('', 'font-family:JetBrains Mono,monospace;font-size:13.5px;color:var(--text2);background:var(--bg-input);padding:1px 14px', inner || '<br>');
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
      if (!inTable) {
        html += '<div class="tbl-scroll"><div class="tbl-grid">';
        inTable = true;
      }
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
      // One `.cm-quote` div per quote line (the `>` stays in a hidden cm-mark so
      // the source round-trips). All styling — including the left accent bar — is
      // in the `.cm-quote` CSS class, which carries NO vertical margin between
      // consecutive quote lines, so their bars meet into ONE continuous line.
      html += div('cm-quote', '', mk(m[1]) + (inlineMd(m[2]) || '<br>'));
      continue;
    }
    if (/^(-{3,}|\*{3,})\s*$/.test(ln)) {
      html += div('', 'border-bottom:1px solid var(--line);line-height:1;padding-bottom:11px;margin:8px 0', mk(esc(ln)));
      continue;
    }
    if ((m = ln.match(/^(\s*)([-*])(\s+)(\[[ xX]\])(\s+)(.*)$/))) {
      const done = /[xX]/.test(m[4]);
      const ind = m[1].length ? 'margin-left:' + m[1].length * 0.55 + 'em' : '';
      // A real (zero-width-text) element over the visual checkbox so clicks have a
      // deterministic hit target. It holds NO source text, so readLive round-trips
      // unchanged; LiveEditor toggles `[ ]`↔`[x]` when this element is clicked.
      const box = '<span class="cm-task-box" contenteditable="false" aria-hidden="true"></span>';
      html += div('cm-task' + (done ? ' cm-task-done' : ''), ind, box + mk(m[1] + m[2] + m[3] + m[4] + m[5]) + '<span class="cm-body">' + (inlineMd(m[6]) || '<br>') + '</span>');
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
  closeTable();
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
// LiveEditor's single-line re-render works). One shared, config-driven tokenizer
// powers most languages; HTML and CSS get bespoke per-line scanners for their
// markup/selector grammar. The contract is strict: every emitted <div>'s
// textContent MUST equal the original source line verbatim — we only ever wrap
// substrings in <span> wrappers (never insert or drop characters), so the editor
// can read the source back out of the DOM unchanged.

// Per-language config. `kw` are keywords, `types` render as types, `lit` are
// literal constants (true/false/null/None…). `line`/`block` describe comment
// syntax (block = C-style /* */). `quotes` lists string delimiters. `ci` makes
// keyword matching case-insensitive (SQL). `upType` highlights Capitalized
// identifiers as types (Rust/TS). `key` marks `name:`/`name=` keys at line start.
// `strKey` highlights a "quoted" key followed by `:` (JSON).
type LangCfg = {
  kw: string;
  types?: string;
  lit?: string;
  line?: string[];
  block?: boolean;
  quotes?: string;
  ci?: boolean;
  upType?: boolean;
  key?: ':' | '=';
  strKey?: boolean;
};

const LANGS: Record<string, LangCfg> = {
  js: {
    kw: 'import export from default const let var function return async await class extends super new delete typeof instanceof void this if else for while do switch case break continue try catch finally throw yield in of interface type enum implements public private protected readonly abstract static get set as satisfies keyof infer namespace declare is override module require',
    lit: 'true false null undefined NaN Infinity',
    line: ['//'],
    block: true,
    upType: true,
  },
  json: { kw: '', lit: 'true false null', quotes: '"', strKey: true },
  py: {
    kw: 'and as assert async await break class continue def del elif else except finally for from global if import in is lambda nonlocal not or pass raise return try while with yield match case',
    types: 'int str float bool list dict set tuple bytes bytearray object complex frozenset range type self cls',
    lit: 'True False None',
    line: ['#'],
  },
  rs: {
    kw: 'as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return static struct super trait type union unsafe use where while macro_rules',
    types: 'i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128 usize f32 f64 bool char str String Vec Option Result Box Rc Arc HashMap HashSet Cow Self',
    lit: 'true false',
    line: ['//'],
    block: true,
    upType: true,
  },
  sh: {
    kw: 'if then elif else fi for while until do done case esac function return export local readonly declare alias source eval echo printf read cd exit set unset trap shift test',
    line: ['#'],
  },
  yaml: { kw: 'true false null yes no on off True False Null Yes No On Off', lit: 'true false null', line: ['#'], key: ':' },
  toml: { kw: '', lit: 'true false', line: ['#'], key: '=' },
  sql: {
    kw: 'select from where insert into values update set delete create table drop alter add column primary key foreign references index unique view join inner left right outer full on group by order asc desc having limit offset union all distinct as and or not null is in like between exists count sum avg min max case when then else end begin commit rollback transaction with default constraint check cascade',
    lit: 'null true false',
    line: ['--'],
    block: true,
    ci: true,
    quotes: '\'"',
  },
  txt: { kw: '' },
};

// Value keywords for CSS (properties are detected by the trailing `:`, so this is
// the set of common value identifiers worth highlighting).
const CSS_KW = new Set(
  ('inherit initial unset revert auto none flex grid block inline inline-block absolute relative fixed static sticky bold normal italic center left right justify solid dashed dotted hidden visible pointer transparent currentColor border-box content-box wrap nowrap row column uppercase lowercase capitalize').split(' ')
);

const C = {
  cmt: 'var(--faint)',
  str: '#3a7d4d',
  num: '#b6612e',
  kw: '#8250df',
  lit: '#b6612e',
  key: '#2563eb',
  fn: '#7c5cff',
  type: '#1f9aa0',
  tag: '#22863a',
  attr: '#6f42c1',
  sel: '#0a8f5b',
  prop: '#2563eb',
};
const sp = (c: string, t: string) => '<span style="color:' + c + '">' + t + '</span>';
const cmtSpan = (t: string) => sp(C.cmt, '<span style="font-style:italic">' + esc(t) + '</span>');
const reEsc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

// Build the shared tokenizer regex for a language config. Groups (stable order):
// 1=comment 2=string 3=number 4=identifier. When the language has no comment
// syntax the comment group is an always-empty `()` concatenated onto the string
// group (so it never matches on its own and can't cause a zero-width loop).
function buildRe(cfg: LangCfg): string {
  const cmt: string[] = [];
  if (cfg.block) cmt.push('/\\*.*?\\*/|/\\*.*');
  if (cfg.line) for (const l of cfg.line) cmt.push(reEsc(l) + '[^\\n]*');
  const cmtGroup = cmt.length ? '(' + cmt.join('|') + ')|' : '()';
  const quotes = cfg.quotes ?? '"\'`';
  const strs = [...quotes].map((q) => {
    const e = reEsc(q);
    return e + '(?:[^' + e + '\\\\]|\\\\.)*' + e;
  });
  return cmtGroup + '(' + strs.join('|') + ')|(\\b\\d[\\d._eExXa-fA-F]*\\b)|([A-Za-z_$][\\w$]*)';
}

function genericLine(raw: string, cfg: LangCfg, reSrc: string): string {
  const kw = new Set(cfg.kw.split(/\s+/).filter(Boolean));
  const types = new Set((cfg.types || '').split(/\s+/).filter(Boolean));
  const lits = new Set((cfg.lit || '').split(/\s+/).filter(Boolean));
  const re = new RegExp(reSrc, 'g');
  let out = '';
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(raw)) !== null) {
    out += esc(raw.slice(last, m.index));
    last = re.lastIndex;
    const after = raw.slice(re.lastIndex);
    if (m[1]) out += cmtSpan(m[1]);
    else if (m[2]) out += sp(cfg.strKey && /^\s*:/.test(after) ? C.key : C.str, esc(m[2]));
    else if (m[3]) out += sp(C.num, esc(m[3]));
    else {
      const w = m[4];
      const cw = cfg.ci ? w.toLowerCase() : w;
      if (kw.has(cw)) out += sp(C.kw, esc(w));
      else if (lits.has(w)) out += sp(C.lit, esc(w));
      else if (types.has(w) || (cfg.upType && /^[A-Z]/.test(w))) out += sp(C.type, esc(w));
      else if (cfg.key && raw.slice(0, m.index).trim() === '' && new RegExp('^\\s*' + reEsc(cfg.key)).test(after)) out += sp(C.key, esc(w));
      else if (/^\s*\(/.test(after)) out += sp(C.fn, esc(w));
      else out += esc(w);
    }
  }
  out += esc(raw.slice(last));
  return out;
}

// HTML per-line scanner. Groups: 1=comment 2=tag-punct 3=tag-name 4=tag-close
// 5=string 6=entity 7=word. `inTag` tracks whether a bare word is an attribute.
const HTML_RE = /(<!--.*?-->|<!--.*)|(<\/?)([A-Za-z][\w:-]*)|(\/?>)|("[^"]*"|'[^']*')|(&[#\w]+;)|([A-Za-z_:][\w.:-]*)/;
function htmlLine(raw: string): string {
  const re = new RegExp(HTML_RE.source, 'g');
  let out = '';
  let last = 0;
  let inTag = false;
  let m: RegExpExecArray | null;
  while ((m = re.exec(raw)) !== null) {
    out += esc(raw.slice(last, m.index));
    last = re.lastIndex;
    if (m[1]) out += cmtSpan(m[1]);
    else if (m[3] !== undefined) {
      inTag = true;
      out += sp(C.tag, esc(m[2] + m[3]));
    } else if (m[4]) {
      inTag = false;
      out += sp(C.tag, esc(m[4]));
    } else if (m[5]) out += sp(C.str, esc(m[5]));
    else if (m[6]) out += sp(C.lit, esc(m[6]));
    else out += inTag ? sp(C.attr, esc(m[7])) : esc(m[7]);
  }
  out += esc(raw.slice(last));
  return out;
}

// CSS per-line scanner. Groups: 1=comment 2=string 3=at-rule 4=!important
// 5=hex-colour 6=number(+unit) 7=identifier (selector / property / value).
const CSS_RE = /(\/\*.*?\*\/|\/\*.*)|("[^"]*"|'[^']*')|(@[\w-]+)|(!\s*important)|(#[0-9a-fA-F]{3,8}\b)|(-?\d[\d.]*(?:px|em|rem|ex|ch|vw|vh|vmin|vmax|fr|deg|s|ms|pt|cm|mm|in|%)?)|([.#]?-?[A-Za-z_][\w-]*)/;
function cssLine(raw: string): string {
  const re = new RegExp(CSS_RE.source, 'g');
  let out = '';
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(raw)) !== null) {
    const before = raw.slice(0, m.index);
    out += esc(raw.slice(last, m.index));
    last = re.lastIndex;
    if (m[1]) out += cmtSpan(m[1]);
    else if (m[2]) out += sp(C.str, esc(m[2]));
    else if (m[3]) out += sp(C.kw, esc(m[3]));
    else if (m[4]) out += sp(C.kw, esc(m[4]));
    else if (m[5]) out += sp(C.num, esc(m[5]));
    else if (m[6]) out += sp(C.num, esc(m[6]));
    else {
      const w = m[7];
      if (w[0] === '.' || w[0] === '#') out += sp(C.sel, esc(w));
      // A property is an identifier that opens a declaration: it is followed by
      // `:` and preceded only by the line start, a `{`/`;`, or a closed comment
      // (so `a:hover` reads as a pseudo-class selector, not a property).
      else if (/(?:^|[{;]|\*\/)\s*$/.test(before) && /^\s*:/.test(raw.slice(re.lastIndex))) out += sp(C.prop, esc(w));
      else if (CSS_KW.has(w)) out += sp(C.kw, esc(w));
      else out += esc(w);
    }
  }
  out += esc(raw.slice(last));
  return out;
}

// Map a fenced-code info string (```tsx, ```python, ```rust …) to a highlighter
// language key. Returns 'txt' for none/unknown, so callers can skip highlighting.
export function fenceLang(info: string): string {
  const w = (String(info || '').trim().toLowerCase().match(/^[a-z0-9+#.]+/) || [''])[0];
  if (['ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs', 'javascript', 'typescript'].includes(w)) return 'js';
  if (['py', 'python'].includes(w)) return 'py';
  if (['rs', 'rust'].includes(w)) return 'rs';
  if (['sh', 'bash', 'zsh', 'shell', 'console'].includes(w)) return 'sh';
  if (['yml', 'yaml'].includes(w)) return 'yaml';
  if (w === 'toml') return 'toml';
  if (w === 'sql') return 'sql';
  if (['json', 'jsonc', 'json5'].includes(w)) return 'json';
  if (['html', 'htm', 'xml', 'svg', 'vue'].includes(w)) return 'html';
  if (['css', 'scss', 'sass', 'less'].includes(w)) return 'css';
  return 'txt';
}

// One per-line highlighter for a resolved language key — shared by the code-file
// view and markdown fenced blocks. Every line it emits preserves textContent
// verbatim (only wraps substrings in spans), so the editor reads the source back
// out of the DOM unchanged.
function lineHighlighterFor(langKey: string): (raw: string) => string {
  const cfg = LANGS[langKey] || LANGS.txt;
  const reSrc = langKey === 'html' || langKey === 'css' ? '' : buildRe(cfg);
  return langKey === 'html' ? htmlLine : langKey === 'css' ? cssLine : (raw: string) => genericLine(raw, cfg, reSrc);
}

export function renderCodeHtml(src: string, lang: string): string {
  const line = lineHighlighterFor(lang);
  return String(src)
    .replace(/\r/g, '')
    .split('\n')
    .map((raw) => {
      const inner = line(raw);
      return '<div>' + (inner === '' ? '<br>' : inner) + '</div>';
    })
    .join('');
}

// Render whatever the selected file is: code highlighter for code files, the live
// markdown renderer otherwise.
export function renderDoc(src: string, path: string, accent: string, fmStyle: FrontmatterStyle): string {
  return isCodeFile(path) ? renderCodeHtml(src, langOf(path)) : renderLiveHtml(src, accent, fmStyle);
}

// The editor's line nodes, flattening any `.tbl-scroll` table grouping back into
// its constituent `.tbl-row` children. Tables render as a single top-level
// `.tbl-scroll > .tbl-grid > .tbl-row` nesting (so only the table scrolls
// horizontally and its columns align), but each row is still exactly one source
// line — so callers walk this flattened list to keep the strict 1:1 line↔node
// mapping the source reconstruction and caret math depend on. We descend through
// BOTH wrappers to the grid and expose ONLY its `.tbl-row` children; the trailing
// `.tbl-pad` scroll spacer is a sibling of the grid and maps to no source line.
function lineNodes(el: HTMLElement): ChildNode[] {
  const out: ChildNode[] = [];
  el.childNodes.forEach((n) => {
    // A rendered diagram preview is a contenteditable=false sibling that maps to
    // NO source line — skip it entirely so readLive/caretOffset/setCaret treat it
    // as if it weren't there (zero lines, zero characters).
    if (n.nodeType === 1 && (n as HTMLElement).classList?.contains('md-diagram')) return;
    if (n.nodeType === 1 && (n as HTMLElement).classList?.contains('tbl-scroll')) {
      const grid = (n as HTMLElement).querySelector('.tbl-grid');
      if (grid) grid.childNodes.forEach((r) => out.push(r));
    } else {
      out.push(n);
    }
  });
  return out;
}

// Reconstruct source markdown from the editor DOM: one source line per line
// node (its textContent, which includes the hidden cm-mark syntax); <br> → "".
export function readLive(el: HTMLElement): string {
  const out: string[] = [];
  lineNodes(el).forEach((n) => {
    if (n.nodeType === 3) out.push(n.nodeValue || '');
    else if ((n as HTMLElement).nodeName === 'BR') out.push('');
    else out.push((n as HTMLElement).textContent || '');
  });
  return out.join('\n');
}

// Map a DOM (container, offset) boundary that lies within one line node to a
// character index, counting ALL text in that line (including the hidden cm-mark
// markers). Crucially this handles boundaries that land on an *element* rather
// than a text node — clicking the flex gap between a frontmatter key and value,
// clearing a value (leaving the caret on the empty value span), or selecting a
// range all leave the selection anchored to an element with a child-index
// offset. Counting that as the start (or end) of the line is what made the caret
// jump to the wrong frontmatter field; here we count exactly the text that
// precedes the boundary in document order so the column is preserved.
function localCaretOffset(line: Node, container: Node, endOffset: number): number {
  // Boundary directly on a bare text-node line: the offset IS the column.
  if (container === line && line.nodeType === 3) return endOffset;
  const range = document.createRange();
  range.setStart(line, 0);
  range.setEnd(container, endOffset);
  let len = 0;
  const w = document.createTreeWalker(line, NodeFilter.SHOW_TEXT);
  let n: Node | null;
  while ((n = w.nextNode())) {
    // The boundary sits inside this text node (a normal caret-in-text click).
    if (n === container) return len + endOffset;
    const tlen = (n.nodeValue || '').length;
    // comparePoint ≤ 0 ⇒ the node ends at/before the boundary ⇒ it's entirely to
    // the left of the caret and counts. The first node that ends after the
    // boundary (and every node past it) is to the right, so we stop.
    if (range.comparePoint(n, tlen) <= 0) len += tlen;
    else return len;
  }
  return len;
}

// Flatten the current selection to a character offset across the line-divs
// (+1 per line boundary), so a re-render can restore the caret.
export function caretOffset(el: HTMLElement): number | null {
  const sel = getSelection();
  if (!sel || !sel.rangeCount) return null;
  const r = sel.getRangeAt(0);
  const container = r.endContainer as Node;
  let offset = 0;
  const kids = lineNodes(el);
  for (let i = 0; i < kids.length; i++) {
    const child = kids[i] as HTMLElement;
    if (i > 0) offset += 1;
    if (child === container || (child.nodeType !== 3 && child.contains(container))) {
      return offset + localCaretOffset(child, container, r.endOffset);
    }
    offset += child.nodeType === 3 ? (child.nodeValue || '').length : child.nodeName === 'BR' ? 0 : (child.textContent || '').length;
  }
  return offset;
}

export function setCaret(el: HTMLElement, target: number): void {
  let remaining = target;
  const kids = lineNodes(el);
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
