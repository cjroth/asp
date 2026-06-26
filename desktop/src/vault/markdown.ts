// Live (contentEditable) Markdown WYSIWYG — a faithful port of the design's
// `renderLiveHtml`/`readLive`/caret helpers. Each source line becomes one
// top-level <div> (empty line → <br>), and literal markdown syntax is wrapped
// in hidden `.cm-mark` spans so it round-trips through `readLive` while staying
// invisible. This 1:1 line↔div mapping is what keeps caret math stable.

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

export function renderLiveHtml(src: string, accent = '#3d63dd'): string {
  const lines = String(src).replace(/\r/g, '').split('\n');
  const div = (cls: string, style: string, inner: string) =>
    '<div' + (cls ? ' class="' + cls + '"' : '') + (style ? ' style="' + style + '"' : '') + '>' + (inner === '' ? '<br>' : inner) + '</div>';
  let html = '';
  let inFence = false;
  for (let i = 0; i < lines.length; i++) {
    const ln = lines[i];
    if (/^```/.test(ln)) {
      inFence = !inFence;
      html += div('', 'background:#faf9f5;padding:4px 14px;border-radius:' + (inFence ? '9px 9px 0 0' : '0 0 9px 9px'), mk(esc(ln)));
      continue;
    }
    if (inFence) {
      html += div('', 'font-family:JetBrains Mono,monospace;font-size:13.5px;color:#44403c;background:#faf9f5;padding:1px 14px', esc(ln) || '<br>');
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
      html += div('', 'border-left:3px solid ' + accent + '55;padding:1px 0 1px 14px;color:#6b6760;font-style:italic;margin:2px 0', mk(m[1]) + (inlineMd(m[2]) || '<br>'));
      continue;
    }
    if (/^(-{3,}|\*{3,})\s*$/.test(ln)) {
      html += div('', 'border-bottom:1px solid #e3e0db;line-height:1;padding-bottom:11px;margin:8px 0', mk(esc(ln)));
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

export function wordCountOf(content: string): string {
  const words = content.trim() ? content.trim().split(/\s+/).length : 0;
  return words + (words === 1 ? ' word' : ' words');
}
