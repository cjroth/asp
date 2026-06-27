// Customize-vault modal: set a custom name, color, and emoji/monogram icon. The
// result is cosmetic metadata persisted locally (see vaultMeta.ts) and overlaid
// on the real vault — it never touches the protocol. Faithful to the design's
// Customize modal (emoji category tabs + search + grid, live avatar preview).
import React, { useState } from 'react';
import { EMOJI_CATEGORIES, emojiResults } from './emoji';
import * as Icon from './icons';
import { avatarStyle, glyphOf, HUES } from './vaultMeta';

export interface CustomizeInit {
  id: string;
  name: string;
  hue: number;
  emoji: string | null;
}

export interface CustomizeModalProps {
  initial: CustomizeInit;
  onSave: (meta: CustomizeInit) => void;
  onCancel: () => void;
}

export default function CustomizeModal({ initial, onSave, onCancel }: CustomizeModalProps) {
  const [name, setName] = useState(initial.name);
  const [hue, setHue] = useState(initial.hue);
  const [emoji, setEmoji] = useState<string | null>(initial.emoji);
  const [search, setSearch] = useState('');
  const [cat, setCat] = useState(0);

  const query = search.trim();
  const results = emojiResults(search, cat);

  const save = () => onSave({ id: initial.id, name: name.trim() || 'Untitled vault', hue, emoji });

  return (
    <>
      <div onClick={onCancel} style={{ position: 'fixed', inset: 0, zIndex: 74, background: 'var(--overlay)', backdropFilter: 'blur(2px)' }} />
      <div
        onKeyDown={(e) => {
          if (e.key === 'Escape') { e.preventDefault(); onCancel(); }
          else if (e.key === 'Enter') { e.preventDefault(); save(); }
        }}
        style={{ position: 'fixed', zIndex: 75, top: '50%', left: '50%', transform: 'translate(-50%,-50%)', width: 'min(424px,92vw)', maxHeight: '92vh', overflowY: 'auto', background: 'var(--bg)', borderRadius: 16, boxShadow: '0 24px 60px rgba(28,25,23,0.28)', padding: 20, display: 'flex', flexDirection: 'column', gap: 16 }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 13 }}>
          <div style={avatarStyle({ hue, emoji }, 46, 13)}>{glyphOf({ emoji, name })}</div>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontSize: 16, fontWeight: 600, letterSpacing: '-0.01em' }}>Customize vault</div>
            <div style={{ fontSize: 12.5, color: 'var(--text3)', marginTop: 2 }}>Set a name, color, and icon.</div>
          </div>
        </div>

        <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
          <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)' }}>Name</span>
          <input
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            spellCheck={false}
            style={{ fontFamily: 'inherit', fontSize: 14, color: 'var(--text)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '10px 12px', outline: 'none', width: '100%', boxSizing: 'border-box' }}
          />
        </label>

        <div style={{ display: 'flex', flexDirection: 'column', gap: 9 }}>
          <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)' }}>Color</span>
          <div style={{ display: 'flex', gap: 10, flexWrap: 'wrap' }}>
            {HUES.map((h) => (
              <div
                key={h}
                data-testid={`swatch-${h}`}
                onClick={() => setHue(h)}
                style={{ width: 28, height: 28, borderRadius: 8, cursor: 'pointer', background: `hsl(${h} 52% 84%)`, boxShadow: hue === h ? `inset 0 0 0 2px var(--bg), 0 0 0 2px hsl(${h} 45% 48%)` : `inset 0 0 0 1px hsl(${h} 36% 74%)`, transition: 'box-shadow .1s' }}
              />
            ))}
          </div>
        </div>

        <div style={{ display: 'flex', flexDirection: 'column', gap: 9 }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)', flex: 1 }}>Icon</span>
            {emoji && (
              <div className="asp-icon-btn" onClick={() => setEmoji(null)} style={{ display: 'flex', alignItems: 'center', gap: 5, fontSize: 12, fontWeight: 500, padding: '4px 9px', borderRadius: 7, cursor: 'pointer', color: 'var(--text3)', background: 'transparent', border: '1px solid var(--line)' }}>
                <Icon.XIcon size={11} stroke="currentColor" />
                <span>Remove icon</span>
              </div>
            )}
          </div>
          <div style={{ border: '1px solid var(--line)', borderRadius: 12, overflow: 'hidden', background: 'var(--bg)', display: 'flex', flexDirection: 'column', height: 214 }}>
            <div style={{ padding: '7px 8px', borderBottom: '1px solid var(--line)', flex: 'none' }}>
              <input
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                spellCheck={false}
                placeholder="Search emojis"
                style={{ fontFamily: 'inherit', fontSize: 12.5, color: 'var(--text)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 7, padding: '4px 10px', outline: 'none', width: '100%', boxSizing: 'border-box' }}
              />
            </div>
            {!query && (
              <div style={{ display: 'flex', gap: 1, padding: '5px 6px', borderBottom: '1px solid var(--line)', flex: 'none' }}>
                {EMOJI_CATEGORIES.map((c, i) => (
                  <div
                    key={c.name}
                    title={c.name}
                    onClick={() => { setCat(i); setSearch(''); }}
                    style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', flex: 1, height: 28, borderRadius: 7, cursor: 'pointer', fontSize: 16, lineHeight: 1, background: i === cat ? 'var(--line)' : 'transparent', transition: 'background .1s' }}
                  >
                    {c.icon}
                  </div>
                ))}
              </div>
            )}
            <div className="asp-scroll" style={{ flex: 1, minHeight: 0, overflowY: 'auto', padding: '6px 7px 8px' }}>
              {results.length > 0 ? (
                <div style={{ display: 'grid', gridTemplateColumns: 'repeat(8, 1fr)', gap: 1 }}>
                  {results.map((ch, i) => (
                    <div key={ch + i} className="asp-hover-list" onClick={() => setEmoji(ch)} style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: 34, borderRadius: 7, cursor: 'pointer', fontSize: 21, lineHeight: 1 }}>
                      {ch}
                    </div>
                  ))}
                </div>
              ) : (
                <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', minHeight: 120, fontSize: 13, color: 'var(--faint)' }}>No emoji found</div>
              )}
            </div>
          </div>
        </div>

        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8, marginTop: 2 }}>
          <button onClick={onCancel} style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--text2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 9, padding: '8px 16px', cursor: 'pointer' }}>Cancel</button>
          <button onClick={save} style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--bg)', background: 'var(--text)', border: 'none', borderRadius: 9, padding: '8px 18px', cursor: 'pointer' }}>Save</button>
        </div>
      </div>
    </>
  );
}
