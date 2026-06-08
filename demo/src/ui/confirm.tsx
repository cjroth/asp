/* ====================================================================
   confirm.tsx · imperative confirmation modal
   --------------------------------------------------------------------
   <ConfirmHost/> is mounted once at the app root; anywhere else, call
   confirmDialog({...}) and await the boolean. Styled like the demo's
   blueprint dialogs (square, cyan corner brackets), Enter=confirm,
   Esc / click-outside = cancel. Falls back to window.confirm if the
   host isn't mounted (e.g. SSR smoke test).
   ==================================================================== */
import React, { useEffect, useState } from 'react';

export interface ConfirmOpts {
  title: string;
  message: React.ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
}

type Pending = { opts: ConfirmOpts; resolve: (v: boolean) => void };

let _show: ((opts: ConfirmOpts) => Promise<boolean>) | null = null;

export function confirmDialog(opts: ConfirmOpts): Promise<boolean> {
  if (_show) return _show(opts);
  return Promise.resolve(typeof window !== 'undefined' && window.confirm
    ? window.confirm(typeof opts.message === 'string' ? opts.message : opts.title)
    : true);
}

export function ConfirmHost() {
  const [pending, setPending] = useState<Pending | null>(null);

  useEffect(() => {
    _show = (opts) => new Promise<boolean>((resolve) => setPending({ opts, resolve }));
    return () => { _show = null; };
  }, []);

  useEffect(() => {
    if (!pending) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') close(false);
      if (e.key === 'Enter') close(true);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  if (!pending) return null;
  const { opts } = pending;
  const close = (v: boolean) => { pending.resolve(v); setPending(null); };

  return (
    <div className="overlay" style={{ zIndex: 70 }} onMouseDown={(e) => { if ((e.target as HTMLElement).classList.contains('overlay')) close(false); }}>
      <div className="dialog confirm-dialog">
        <div className="dialog-head">
          <span className="eyebrow" style={opts.danger ? { color: 'var(--red)' } : undefined}>confirm</span>
          <h3>{opts.title}</h3>
          <p>{opts.message}</p>
        </div>
        <div className="dialog-foot">
          <button className="btn ghost" onClick={() => close(false)}>{opts.cancelLabel || 'Cancel'}</button>
          <button className={`btn ${opts.danger ? 'danger' : 'primary'}`} autoFocus onClick={() => close(true)}>
            {opts.confirmLabel || 'Confirm'}
          </button>
        </div>
      </div>
    </div>
  );
}
