// In-app log viewer. On mobile Obsidian the WebView dev console is unreachable,
// so this modal is the only way to read — and copy — what the plugin is doing.
// Opened from the "Log" row in settings; streams live while open.

import { type App, Modal, Notice } from 'obsidian';
import type { LogBuffer, LogEntry } from './log-buffer.ts';

export class LogModal extends Modal {
  private unsub: (() => void) | null = null;
  private body?: HTMLElement;

  constructor(
    app: App,
    private readonly buffer: LogBuffer,
  ) {
    super(app);
  }

  onOpen(): void {
    const c = this.contentEl;

    const title = document.createElement('h2');
    title.textContent = 'Activity log';
    c.appendChild(title);

    // Native Obsidian modal button row (right-aligned, themed buttons).
    const actions = document.createElement('div');
    actions.className = 'modal-button-container';
    c.appendChild(actions);
    this.button(actions, 'Copy all', () => void this.copyAll(), true);
    this.button(actions, 'Clear', () => {
      this.buffer.clear();
      this.render();
    });
    this.button(actions, 'Close', () => this.close());

    const body = document.createElement('pre');
    Object.assign(body.style, {
      maxHeight: '60vh',
      overflowY: 'auto',
      whiteSpace: 'pre-wrap',
      wordBreak: 'break-word',
      fontFamily: 'var(--font-monospace, monospace)',
      fontSize: '12px',
      lineHeight: '1.45',
      padding: '8px',
      margin: '8px 0 0',
      border: '1px solid var(--background-modifier-border)',
      borderRadius: '6px',
      background: 'var(--background-secondary)',
    } as Partial<CSSStyleDeclaration>);
    this.body = body;
    c.appendChild(body);

    this.render();
    this.unsub = this.buffer.subscribe(() => this.render());
  }

  onClose(): void {
    this.unsub?.();
    this.unsub = null;
    const c = this.contentEl;
    while (c.firstChild) c.removeChild(c.firstChild);
  }

  private button(parent: HTMLElement, text: string, onClick: () => void, cta = false): void {
    const btn = document.createElement('button');
    btn.textContent = text;
    if (cta) btn.classList.add('mod-cta');
    btn.addEventListener('click', onClick);
    parent.appendChild(btn);
  }

  private render(): void {
    const el = this.body;
    if (!el) return;
    const entries = this.buffer.snapshot();
    const fmt = (e: LogEntry) => `[${e.ts}] ${e.level === 'error' ? 'ERR ' : ''}${e.msg}`;
    el.textContent = entries.length ? entries.map(fmt).join('\n') : '(no activity yet)';
    el.scrollTop = el.scrollHeight;
  }

  private async copyAll(): Promise<void> {
    const text = this.buffer.toText();
    if (!text) {
      new Notice('asp: log is empty');
      return;
    }
    try {
      await navigator.clipboard.writeText(text);
      new Notice(`asp: copied ${text.split('\n').length} log line(s)`);
    } catch (e) {
      new Notice(`asp: copy failed — ${String(e)}`);
    }
  }
}
