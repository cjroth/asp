//! Asset source for bundled SVG icons + fonts. Reads from the crate's `assets/`
//! dir (path baked at compile time). Good enough for dev; switch to an embedded
//! source (rust-embed / include_bytes) for shipping.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::Result;
use gpui::{AssetSource, SharedString};

const ASSET_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");

pub struct Assets;

impl Assets {
    fn full(path: &str) -> PathBuf {
        Path::new(ASSET_ROOT).join(path)
    }

    /// The bundled font bytes to register with the text system (mono + serif).
    pub fn font_bytes() -> Vec<Cow<'static, [u8]>> {
        ["fonts/JetBrainsMono.ttf", "fonts/Newsreader.ttf"]
            .iter()
            .filter_map(|p| std::fs::read(Self::full(p)).ok().map(Cow::Owned))
            .collect()
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match std::fs::read(Self::full(path)) {
            Ok(bytes) => Ok(Some(Cow::Owned(bytes))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let dir = Self::full(path);
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                if let Some(name) = e.file_name().to_str() {
                    out.push(SharedString::from(name.to_string()));
                }
            }
        }
        Ok(out)
    }
}
