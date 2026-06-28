//! Cosmetic per-vault metadata (custom name, hue, emoji), ported from desktop
//! `src/vault/vaultMeta.ts`. Pure parts only — persistence is the app layer's job
//! and the avatar CSS lives in `theme.rs`. The djb2 hash matches JS exactly
//! (UTF-16 code units, unsigned 32-bit) so default hues agree across apps.

use std::collections::HashMap;

/// The 8 swatch hues offered in the Customize modal.
pub const HUES: [i64; 8] = [222, 158, 32, 268, 344, 188, 46, 12];

#[derive(Debug, Clone, PartialEq)]
pub struct VaultMetaEntry {
    pub name: Option<String>,
    pub hue: f64,
    pub emoji: Option<String>,
}

pub type VaultMetaMap = HashMap<String, VaultMetaEntry>;

/// djb2 over UTF-16 code units, truncated to unsigned 32-bit (matches JS
/// `((h<<5)+h+charCodeAt(i)) >>> 0`).
pub fn hash(s: &str) -> u32 {
    let mut h: u32 = 5381;
    for unit in s.encode_utf16() {
        h = h.wrapping_shl(5).wrapping_add(h).wrapping_add(unit as u32);
    }
    h
}

pub fn hue_for_id(id: &str) -> u32 {
    hash(id) % 360
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMeta {
    pub name: String,
    pub hue: f64,
    pub emoji: Option<String>,
}

/// Saved overlay if present, else defaults (basename, hash-derived hue, no emoji).
pub fn resolve_meta(map: &VaultMetaMap, vault_id: &str, fallback_name: &str) -> ResolvedMeta {
    let m = map.get(vault_id);
    let name = m
        .and_then(|e| e.name.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback_name.to_string());
    let hue = m.map(|e| e.hue).unwrap_or_else(|| hue_for_id(vault_id) as f64);
    let emoji = m.and_then(|e| e.emoji.clone()).filter(|s| !s.is_empty());
    ResolvedMeta { name, hue, emoji }
}

/// Avatar glyph: emoji if set, else the name's uppercase initial, else a dot.
pub fn glyph_of(emoji: Option<&str>, name: &str) -> String {
    if let Some(e) = emoji.filter(|s| !s.is_empty()) {
        return e.to_string();
    }
    name.trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "·".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_deterministic_and_hue() {
        assert_eq!(hash("abc"), hash("abc"));
        assert_eq!(hue_for_id("vault-x"), hash("vault-x") % 360);
    }

    #[test]
    fn resolve_meta_defaults() {
        let r = resolve_meta(&HashMap::new(), "vid1", "massive");
        assert_eq!(r.name, "massive");
        assert_eq!(r.hue, hue_for_id("vid1") as f64);
        assert_eq!(r.emoji, None);
    }

    #[test]
    fn resolve_meta_overlay() {
        let mut map = HashMap::new();
        map.insert(
            "vid1".to_string(),
            VaultMetaEntry { name: Some("Custom".into()), hue: 32.0, emoji: Some("🚀".into()) },
        );
        let r = resolve_meta(&map, "vid1", "massive");
        assert_eq!(r, ResolvedMeta { name: "Custom".into(), hue: 32.0, emoji: Some("🚀".into()) });
    }

    #[test]
    fn glyph_prefers_emoji_then_initial_then_dot() {
        assert_eq!(glyph_of(Some("🚀"), "Work"), "🚀");
        assert_eq!(glyph_of(None, "work"), "W");
        assert_eq!(glyph_of(None, ""), "·");
    }

    #[test]
    fn hues_count() {
        assert_eq!(HUES.len(), 8);
    }
}
