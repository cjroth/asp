// Cosmetic per-vault metadata — custom name, color hue, and emoji icon. The asp
// protocol has no place for this, and the user chose to keep it purely local, so
// it lives in localStorage keyed by the stable `vault_id`. It only ever *overlays*
// real vault data (the SDK remains the source of truth for path/files/sync).
import type React from 'react';

export interface VaultMetaEntry {
  name?: string;
  hue: number;
  emoji?: string | null;
}
export type VaultMetaMap = Record<string, VaultMetaEntry>;

const KEY = 'asp.vaultmeta.v1';

// The 8 swatch hues offered in the Customize modal (design line 1811).
export const HUES = [222, 158, 32, 268, 344, 188, 46, 12];

// djb2 (matches the design's `hash`) — deterministic default hue from an id.
export function hash(str: string): number {
  let h = 5381;
  for (let i = 0; i < str.length; i++) h = ((h << 5) + h + str.charCodeAt(i)) >>> 0;
  return h;
}
export function hueForId(id: string): number {
  return hash(id) % 360;
}

export function loadVaultMeta(): VaultMetaMap {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) return JSON.parse(raw) as VaultMetaMap;
  } catch {
    /* ignore */
  }
  return {};
}

export function saveVaultMeta(map: VaultMetaMap): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(map));
  } catch {
    /* ignore */
  }
}

export interface ResolvedMeta {
  name: string;
  hue: number;
  emoji: string | null;
}

// Resolve the display metadata for a vault: the saved overlay if present, else
// sensible defaults (basename as the name, a hash-derived hue, no emoji).
export function resolveMeta(map: VaultMetaMap, vaultId: string, fallbackName: string): ResolvedMeta {
  const m = map[vaultId];
  return {
    name: (m && m.name) || fallbackName,
    hue: m ? m.hue : hueForId(vaultId),
    emoji: (m && m.emoji) || null,
  };
}

// The avatar glyph: the emoji if set, else the name's first letter, else a dot.
export function glyphOf(meta: { emoji?: string | null; name?: string }): string {
  return meta.emoji ? meta.emoji : (meta.name || '').trim().charAt(0).toUpperCase() || '·';
}

// The color-tinted rounded-square avatar (emoji or monogram). A faithful port of
// the design's `avatarStyle` (lines 1814–1819); intentionally theme-independent —
// the pastel tile reads the same in light and dark, exactly as the design draws it.
export function avatarStyle(meta: { hue: number; emoji?: string | null }, size: number, radius: number): React.CSSProperties {
  const base: React.CSSProperties = {
    width: size,
    height: size,
    borderRadius: radius,
    flex: 'none',
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'center',
    background: `hsl(${meta.hue} 44% 94%)`,
    border: `1px solid hsl(${meta.hue} 36% 86%)`,
  };
  return meta.emoji
    ? { ...base, fontSize: Math.round(size * 0.54), lineHeight: 1 }
    : { ...base, fontSize: Math.round(size * 0.4), fontWeight: 600, color: `hsl(${meta.hue} 42% 40%)` };
}
