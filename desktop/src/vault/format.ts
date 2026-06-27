// Small pure formatting/util helpers used across the app. Kept out of App.tsx so
// they're unit-testable in isolation.

export const basename = (p: string): string => p.split('/').filter(Boolean).pop() || p;

// A human "time ago" for a wall-clock unix-seconds timestamp (or em-dash if none).
export function relTime(sec: number | null | undefined): string {
  if (!sec) return '—';
  const d = Math.max(0, Math.floor(Date.now() / 1000) - sec);
  if (d < 5) return 'just now';
  if (d < 60) return d + 's ago';
  if (d < 3600) return Math.floor(d / 60) + 'm ago';
  if (d < 86400) return Math.floor(d / 3600) + 'h ago';
  if (d < 172800) return 'yesterday';
  return Math.floor(d / 86400) + 'd ago';
}

// Abbreviate an ssh identity to a short, readable fingerprint.
export function shortFingerprint(identity: string): string {
  const cleaned = identity.replace(/^ssh-\S+\s+/, '').trim();
  if (cleaned.length <= 14) return cleaned;
  return cleaned.slice(0, 8) + '…' + cleaned.slice(-4);
}

// A random XXXX-XXXX-XXXX-XXXX access key (Crockford-ish alphabet, no ambiguous chars).
export function makeAccessKey(): string {
  const alpha = 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789';
  const grp = () => Array.from({ length: 4 }, () => alpha[Math.floor(Math.random() * alpha.length)]).join('');
  return [grp(), grp(), grp(), grp()].join('-');
}

// A free "untitled[-n]<ext>" name given a set of existing sibling names.
export function freeName(siblings: Set<string>, ext: string): string {
  let i = 0;
  let name = 'untitled' + ext;
  while (siblings.has(name)) name = 'untitled-' + ++i + ext;
  return name;
}
