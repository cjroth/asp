// Markdown rendering for the editor — ported from the dc mockup's renderLiveHtml
// (the live wysiwyg surface) and mdToHtml (the read-only preview). Pure, no
// React — returns an HTML string the contentEditable / preview render via
// dangerouslySetInnerHTML. The accent color is the vault's.

export function renderLiveHtml(src: string, accent: string): string {
  const esc = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  const mk = (t: string) => '<span class="cm-mark">' + t + '</span>';
  const inline = (raw: string) => {
    let s = esc(raw);
    s = s.replace(/`([^`]+)`/g, (_m, a) => mk('`') + '<code class="cm-code">' + a + '</code>' + mk('`'));
    s = s.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_m, t, u) => mk('[') + '<span class="cm-link">' + t + '</span>' + mk('](' + u + ')'));
    s = s.replace(/\*\*([^*]+)\*\*/g, (_m, a) => mk('**') + '<strong>' + a + '</strong>' + mk('**'));
    s = s.replace(/(^|[^*\w])\*([^*\n]+)\*/g, (_m, p, a) => p + mk('*') + '<em>' + a + '</em>' + mk('*'));
    return s;
  };
  const lines = String(src).replace(/\r/g, '').split('\n');
  const div = (cls: string, style: string, inner: string) =>
    '<div' + (cls ? ' class="' + cls + '"' : '') + (style ? ' style="' + style + '"' : '') + '>' + (inner === '' ? '<br>' : inner) + '</div>';
  let html = '';
  let inFence = false;
  for (const ln of lines) {
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
      html += div('', 'font-size:' + sz + 'px;font-weight:600;letter-spacing:-0.02em;line-height:1.3;margin:' + (lv === 1 ? '2px' : '18px') + ' 0 4px', mk(m[1] + m[2]) + (inline(m[3]) || '<br>'));
      continue;
    }
    if ((m = ln.match(/^(>\s?)(.*)$/))) {
      html += div('', 'border-left:3px solid ' + accent + '55;padding:1px 0 1px 14px;color:#6b6760;font-style:italic;margin:2px 0', mk(m[1]) + (inline(m[2]) || '<br>'));
      continue;
    }
    if (/^(-{3,}|\*{3,})\s*$/.test(ln)) {
      html += div('', 'border-bottom:1px solid #e3e0db;line-height:1;padding-bottom:11px;margin:8px 0', mk(esc(ln)));
      continue;
    }
    if ((m = ln.match(/^(\s*)([-*])(\s+)(\[[ xX]\])(\s+)(.*)$/))) {
      const done = /[xX]/.test(m[4]);
      const ind = m[1].length ? 'margin-left:' + m[1].length * 0.55 + 'em' : '';
      html += div('cm-task' + (done ? ' cm-task-done' : ''), ind, mk(m[1] + m[2] + m[3] + m[4] + m[5]) + '<span class="cm-body">' + (inline(m[6]) || '<br>') + '</span>');
      continue;
    }
    if ((m = ln.match(/^(\s*)([-*])(\s+)(.*)$/))) {
      const ind = m[1].length ? 'margin-left:' + m[1].length * 0.55 + 'em' : '';
      html += div('cm-ul', ind, mk(m[1] + m[2] + m[3]) + (inline(m[4]) || '<br>'));
      continue;
    }
    if ((m = ln.match(/^(\s*)(\d+\.)(\s+)(.*)$/))) {
      const ind = m[1].length ? 'margin-left:' + m[1].length * 0.55 + 'em' : '';
      html += div('', 'padding-left:0.2em' + (ind ? ';' + ind : ''), mk(m[1]) + '<span style="color:' + accent + ';font-weight:500">' + esc(m[2]) + '</span>' + m[3] + (inline(m[4]) || '<br>'));
      continue;
    }
    html += div('', 'margin:0;line-height:1.8', inline(ln));
  }
  return html;
}

export function mdToHtml(src: string, accent: string): string {
  const esc = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  const line = 'color:#1c1917;';
  const inline = (s: string) =>
    esc(s)
      .replace(/`([^`]+)`/g, '<code style="font-family:JetBrains Mono,monospace;font-size:0.86em;background:#f3f1ec;padding:1px 5px;border-radius:5px">$1</code>')
      .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
      .replace(/(^|[^*])\*([^*]+)\*/g, '$1<em>$2</em>')
      .replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="javascript:void(0)" style="color:' + accent + ';text-decoration:none;border-bottom:1px solid ' + accent + '44">$1</a>');
  const lines = src.replace(/\r/g, '').split('\n');
  const out: string[] = [];
  let i = 0;
  const closeList = (stack: string[]) => {
    while (stack.length) out.push('</' + stack.pop() + '>');
  };
  let listStack: string[] = [];
  while (i < lines.length) {
    const ln = lines[i];
    if (/^```/.test(ln)) {
      closeList(listStack);
      listStack = [];
      const buf: string[] = [];
      i++;
      while (i < lines.length && !/^```/.test(lines[i])) {
        buf.push(esc(lines[i]));
        i++;
      }
      i++;
      out.push('<pre style="background:#faf9f5;border:1px solid #ededea;border-radius:10px;padding:14px 16px;overflow:auto;margin:18px 0"><code style="font-family:JetBrains Mono,monospace;font-size:13px;line-height:1.6;color:#292524">' + buf.join('\n') + '</code></pre>');
      continue;
    }
    let m: RegExpMatchArray | null;
    if ((m = ln.match(/^(#{1,4})\s+(.*)$/))) {
      closeList(listStack);
      listStack = [];
      const lv = m[1].length;
      const sz = [0, 28, 21, 17, 15][lv];
      const mt = lv === 1 ? '2px' : '30px';
      out.push('<h' + lv + ' style="font-size:' + sz + 'px;font-weight:600;letter-spacing:-0.02em;margin:' + mt + ' 0 12px;line-height:1.25;' + line + '">' + inline(m[2]) + '</h' + lv + '>');
      i++;
      continue;
    }
    if (/^>\s?/.test(ln)) {
      closeList(listStack);
      listStack = [];
      const buf: string[] = [];
      while (i < lines.length && /^>\s?/.test(lines[i])) {
        buf.push(inline(lines[i].replace(/^>\s?/, '')));
        i++;
      }
      out.push('<blockquote style="margin:18px 0;padding:2px 0 2px 18px;border-left:3px solid ' + accent + '55;color:#57534e;font-style:italic">' + buf.join('<br>') + '</blockquote>');
      continue;
    }
    if (/^(-{3,}|\*{3,})\s*$/.test(ln)) {
      closeList(listStack);
      listStack = [];
      out.push('<hr style="border:none;border-top:1px solid #ededea;margin:26px 0">');
      i++;
      continue;
    }
    if ((m = ln.match(/^(\s*)([-*])\s+(.*)$/))) {
      const item = m[3].replace(/^\[([ xX])\]\s+(.*)$/, (_full, chk, txt) => {
        const done = chk.toLowerCase() === 'x';
        return '<span style="display:inline-flex;align-items:center;gap:9px"><span style="width:15px;height:15px;border-radius:4px;border:1.5px solid ' + (done ? accent : '#d6d3cd') + ';background:' + (done ? accent : 'transparent') + ';display:inline-flex;align-items:center;justify-content:center;flex:none">' + (done ? '<svg width="9" height="9" viewBox="0 0 10 10" fill="none" stroke="white" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M1.5 5 L4 7.5 L8.5 2.5"></path></svg>' : '') + '</span><span style="' + (done ? 'color:#a8a29e;text-decoration:line-through' : '') + '">' + inline(txt) + '</span></span>';
      });
      if (listStack[listStack.length - 1] !== 'ul') {
        closeList(listStack);
        listStack = ['ul'];
        out.push('<ul style="margin:12px 0;padding-left:22px;display:flex;flex-direction:column;gap:6px">');
      }
      out.push('<li style="' + line + '">' + (item === m[3] ? inline(m[3]) : item) + '</li>');
      i++;
      continue;
    }
    if ((m = ln.match(/^(\s*)\d+\.\s+(.*)$/))) {
      if (listStack[listStack.length - 1] !== 'ol') {
        closeList(listStack);
        listStack = ['ol'];
        out.push('<ol style="margin:12px 0;padding-left:24px;display:flex;flex-direction:column;gap:6px">');
      }
      out.push('<li style="' + line + '">' + inline(m[2]) + '</li>');
      i++;
      continue;
    }
    if (/^\|.*\|/.test(ln)) {
      closeList(listStack);
      listStack = [];
      const rows: string[] = [];
      while (i < lines.length && /^\|.*\|/.test(lines[i])) {
        rows.push(lines[i]);
        i++;
      }
      const cells = (r: string) => r.split('|').slice(1, -1).map((c) => c.trim());
      const head = cells(rows[0]);
      const body = rows.slice(2);
      let t = '<table style="border-collapse:collapse;margin:18px 0;font-size:13.5px"><thead><tr>' + head.map((h) => '<th style="text-align:left;padding:7px 14px;border-bottom:2px solid #ededea;font-weight:600">' + inline(h) + '</th>').join('') + '</tr></thead><tbody>';
      for (const r of body) {
        t += '<tr>' + cells(r).map((c) => '<td style="padding:7px 14px;border-bottom:1px solid #f0efec;color:#44403c">' + inline(c) + '</td>').join('') + '</tr>';
      }
      t += '</tbody></table>';
      out.push(t);
      continue;
    }
    if (/^\s*$/.test(ln)) {
      closeList(listStack);
      listStack = [];
      i++;
      continue;
    }
    closeList(listStack);
    listStack = [];
    const para = [ln];
    i++;
    while (i < lines.length && !/^\s*$/.test(lines[i]) && !/^(#{1,4}\s|>\s?|```|\||(\s*[-*]\s)|(\s*\d+\.\s))/.test(lines[i])) {
      para.push(lines[i]);
      i++;
    }
    out.push('<p style="margin:14px 0;line-height:1.72;' + line + '">' + para.map(inline).join('<br>') + '</p>');
  }
  closeList(listStack);
  return out.join('');
}
