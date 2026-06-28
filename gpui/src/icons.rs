//! Icon helper — renders a bundled SVG tinted by `color` at `size`. Names match
//! the files in `assets/icons/` (ported from desktop `src/vault/icons.tsx`).

use gpui::{prelude::*, svg, Hsla, Pixels, Svg};

/// An icon element. Add `.text_color(..)` / override size as needed; this sets
/// sensible defaults so call sites read `icon("plus", px(16.), color)`.
pub fn icon(name: &str, size: Pixels, color: impl Into<Hsla>) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .size(size)
        .text_color(color)
        .flex_none()
}
