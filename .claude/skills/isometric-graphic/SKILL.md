---
name: isometric-graphic
description: Create crisp, brand-consistent isometric vector graphics (SVG) — hero/marketing illustrations in the typesafe.ai style: a central structure connected by dotted paths with flowing tokens, floating label chips, and pop-in tags. Use when asked to design an isometric hero, a "connect X to Y" diagram, an axonometric product illustration, or an animated (looping) isometric scene. Produces editable, recolorable, animatable SVG (optionally self-contained with embedded fonts) and documents the external Recraft→Figma→Lottie pipeline as an alternative.
---

# Isometric graphic

Generate isometric vector art that stays **crisp, editable, recolorable, and
animatable** — the qualities a website hero needs. The reference look
(typesafe.ai) is a *precise, brand-consistent, looping vector scene*: a central
structure, satellite objects on floating islands, dotted connectors with little
tokens tracing the paths, and floating label chips that name things.

There is **no single "prompt → finished animated isometric hero" button** that
yields clean, editable, web-ready output. The job is really *artwork* + *motion*
+ *a small assembly step*. This skill gives you two routes to get there.

## Route A — author it as code (default, deterministic, in-environment)

Best when you have a brand to match and want output that is reviewable in git,
recolors by changing a few constants, and animates with plain SVG. No external
account or model needed. `scripts/iso.py` is a tiny, dependency-free isometric
SVG toolkit; `scripts/build_graphic.py` is a full worked example.

### Workflow

1. **Pull the brand.** Read the target's CSS/site for palette, fonts, stroke
   weight, corner radius, shadow style. Match them exactly — that is what makes
   it look designed rather than generated. (For this repo the palette lives in
   `website/src/styles.css`: cream `#f7f1e6`, ink `#221d2c`, purple `#6b4ef0`,
   yellow `#ffce3a`, pink `#ff7eb6`, green `#35c07a`; fonts Space Grotesk +
   Space Mono; 2–3px ink strokes with hard offset shadows.)
2. **Author a build script** using `iso.py` (copy `build_graphic.py` as a
   starting point). Lay out on the isometric grid, not in screen pixels.
3. **Render to PNG and LOOK at it** — this is not optional. Iterate on
   composition, spacing, and collisions by eye:
   ```bash
   python3 scripts/build_graphic.py --no-fonts > /tmp/out.svg
   node scripts/render.js /tmp/out.svg /tmp/out.png 1120 512
   ```
   Then read the PNG. Fix overlaps (labels colliding, tags swallowed by boxes,
   pieces too close), re-render, repeat. Most of the quality is in this loop.
4. **Ship the SVG.** Add embedded fonts for a self-contained, on-brand file
   (drop `--no-fonts`), or keep it lean and let the host page's `@font-face`
   apply.

### `iso.py` cheat sheet

Projection is true 30° axonometric: `+x`→down-right, `+y`→down-left, `+z`→up.
The camera sees each cuboid's top + two front faces.

```python
from iso import Scene, shade
S = Scene(unit=34, origin=(W/2, 300), ink="#221d2c", stroke=3)
S.box(x, y, z, w, d, h, color)          # shaded cuboid (top/left/right auto-derived)
S.dpath(p0, p1, ..., tokens=3,          # dotted iso connector with riding tokens
        token_color="#ffce3a", dur=2.8) #   (animateMotion along the path)
S.tag(x, y, z, "label", dot="#35c07a")  # floating pop-in chip at a 3D anchor
S.top_center(x,y,z,w,d,h) -> cx,cy,dep  # screen coords to place a vector icon on a top
S.icon(cx, cy, depth, "<svg…>")         # screen-space icon group (draw icons as PATHS)
S.raw("<svg…>", depth)                   # escape hatch (cards, bespoke shapes)
print(S.render(W, H, bg, fonts_css))     # painter's-algorithm depth sort → <svg>
```

Design rules that keep it clean:
- **Draw icons/glyphs as vector paths, not font glyphs.** Latin-subset webfonts
  lack `⚡ ◆ ≣` etc., so symbol glyphs render as tofu or fall back
  non-deterministically. `build_graphic.py` shows path icons (bolt, chevron,
  terminal `>_`, stacked bars, diamond).
- **Space objects generously** so dotted connectors are long enough for tokens
  to travel and labels don't collide. Put satellites on thin "island" tiles for
  the floating look.
- **Recolor via constants only** — one base color per object; `shade()` derives
  the top/left/right faces. Swapping the palette should be a top-of-file edit.
- **Animate with SVG:** `animateMotion`+`<mpath>` rides tokens along a path;
  `stroke-dashoffset` gives the "data flowing" dotted effect; keep it looping.

## Route B — AI generation pipeline (when you want painterly polish / a design tool)

Use when you have the accounts and want to hand off to designers, or the scene
is too organic to lay out by hand. Two AI steps + assembly; do **not** try to
one-shot it with a video model (heavy, non-editable, warps geometry and text).

1. **Artwork → SVG with Recraft** (strongest for vectors: lock an isometric/flat
   style, export layered SVG). Alternatives: Kittl, Illustroke, QuiverAI (has an
   API/MCP). Expect several regenerations; complex multi-object scenes usually
   come out as *pieces you assemble*, not one perfect composition.
   Prompt shape: *"isometric vector illustration, 30° axonometric, flat design,
   [brand palette], a central [structure] connected by dotted paths to [objects],
   clean geometric shapes, minimal detail, named editable layers."*
2. **Clean up in Figma** — name layers, draw the dotted connector paths as real
   paths so they can be path-trimmed.
3. **Motion → Lottie with LottieFiles Motion Copilot** (keeps it vector, tiny,
   editable). Import the SVG; use **path-trimming** for the flowing-token effect
   and keyframes for pop-in tags; set to loop; export `.lottie`. It has an MCP
   server, so an assistant can build/edit the animation in natural language.
   (Recraft can also export a simple animated Lottie directly, with less control.)
4. Drop the `.lottie` on the site.

Avoid Route B's image-to-video tools (DomoAI, Pixelcut, Viddo) for a real hero:
you get a heavy MP4/GIF, not vectors — it won't stay sharp, can't be recolored,
and code/text on screens turns to mush. Fine only for a throwaway social clip.
If you later need hover/scroll interactivity rather than a plain loop, reach for
Rive instead of Lottie.

## Honest expectations
- Budget iteration time — the first composition is never the final one, in
  either route.
- Route A is the reliable default in a coding environment; Route B trades
  determinism for a design-tool workflow and painterly styles.

## Files
- `scripts/iso.py` — the isometric SVG toolkit (import this).
- `scripts/build_graphic.py` — worked example: the Thunderbolt "any agent
  backend over ACP" hero. Read it as a template.
- `scripts/render.js` — SVG→PNG via Playwright/Chromium, for the look-and-iterate
  loop.
