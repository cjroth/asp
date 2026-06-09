// A minimal yes/no confirmation modal. Used for destructive-ish actions like
// "Reset sync config" where a stray click shouldn't silently forget the remote.

import { type App, Modal } from 'obsidian';

export class ConfirmModal extends Modal {
  constructor(
    app: App,
    private readonly opts: {
      title: string;
      body: string;
      confirmText: string;
      onConfirm: () => void | Promise<void>;
    },
  ) {
    super(app);
  }

  onOpen(): void {
    const c = this.contentEl;

    const title = document.createElement('h2');
    title.textContent = this.opts.title;
    c.appendChild(title);

    const p = document.createElement('p');
    p.textContent = this.opts.body;
    c.appendChild(p);

    const actions = document.createElement('div');
    actions.className = 'modal-button-container';
    c.appendChild(actions);

    const cancel = document.createElement('button');
    cancel.textContent = 'Cancel';
    cancel.addEventListener('click', () => this.close());
    actions.appendChild(cancel);

    const confirm = document.createElement('button');
    confirm.textContent = this.opts.confirmText;
    confirm.classList.add('mod-warning');
    confirm.addEventListener('click', async () => {
      this.close();
      await this.opts.onConfirm();
    });
    actions.appendChild(confirm);
  }

  onClose(): void {
    const c = this.contentEl;
    while (c.firstChild) c.removeChild(c.firstChild);
  }
}
