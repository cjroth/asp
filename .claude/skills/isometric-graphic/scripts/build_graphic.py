#!/usr/bin/env python3
"""
Thunderbolt hero: "one context home, any agent backend, over ACP".

A worked example of the isometric-graphic skill. A central Thunderbolt hub that
speaks ACP sits on a floating island; four interchangeable agent backends
(Haystack pipeline, Claude Code, Hermes, built-in agent) connect over dotted ACP
paths with flowing tokens; a floating card plugs bring-your-own skills + MCPs
into the hub. All crisp vector — icons are drawn as paths (not font glyphs) so
the output is deterministic on any renderer.

Run:  python3 build_graphic.py [--no-fonts] > out.svg
"""
import sys, os, base64
sys.path.insert(0, os.path.dirname(__file__))
from iso import Scene, shade

# --- brand palette (website/src/styles.css) ---
CREAM, INK = "#f7f1e6", "#221d2c"
PURPLE, PURPLE_D = "#6b4ef0", "#4a2fd2"
YELLOW, PINK, GREEN = "#ffce3a", "#ff7eb6", "#35c07a"
TAN = "#e7dcc4"

W, H = 1120, 512
S = Scene(unit=34, origin=(W/2, 300), ink=INK, stroke=3)

def island(cx, cy, r, color=TAN):
    S.box(cx - r, cy - r, -0.26, 2*r, 2*r, 0.26, color,
          top=shade(color, 1.05), left=shade(color, 0.82), right=shade(color, 0.68))

# ---------- vector icons (screen space, centered at origin) ----------
def i_bolt(k, fill="#fff"):
    p = [(0.28,-1.0),(-0.48,0.16),(0.02,0.16),(-0.24,1.0),(0.52,-0.22),(0.02,-0.22)]
    pts = " ".join(f"{x*k:.1f},{y*k:.1f}" for x, y in p)
    return f'<polygon points="{pts}" fill="{fill}" stroke="{INK}" stroke-width="2.4" stroke-linejoin="round"/>'

def i_stack(k, fill="#fff"):     # haystack pipeline: 3 stacked bars
    bars, y = "", -0.9*k
    for wgt in (1.0, 0.72, 0.44):
        bw = wgt*1.5*k
        bars += (f'<rect x="{-bw/2:.1f}" y="{y:.1f}" width="{bw:.1f}" height="{0.42*k:.1f}" '
                 f'rx="{0.14*k:.1f}" fill="{fill}" stroke="{INK}" stroke-width="2.4"/>')
        y += 0.62*k
    return bars

def i_term(k, fill="#fff"):      # claude code: >_ prompt
    return (f'<polyline points="{-0.55*k:.1f},{-0.6*k:.1f} {0.0*k:.1f},0 {-0.55*k:.1f},{0.6*k:.1f}" '
            f'fill="none" stroke="{fill}" stroke-width="{0.34*k:.1f}" stroke-linecap="round" stroke-linejoin="round"/>'
            f'<line x1="{0.18*k:.1f}" y1="{0.6*k:.1f}" x2="{0.75*k:.1f}" y2="{0.6*k:.1f}" '
            f'stroke="{fill}" stroke-width="{0.34*k:.1f}" stroke-linecap="round"/>')

def i_wing(k, fill="#fff"):      # hermes: winged chevron
    return (f'<polyline points="{-0.75*k:.1f},{0.5*k:.1f} 0,{-0.7*k:.1f} {0.75*k:.1f},{0.5*k:.1f}" '
            f'fill="none" stroke="{fill}" stroke-width="{0.32*k:.1f}" stroke-linecap="round" stroke-linejoin="round"/>'
            f'<line x1="0" y1="{-0.2*k:.1f}" x2="0" y2="{0.75*k:.1f}" '
            f'stroke="{fill}" stroke-width="{0.32*k:.1f}" stroke-linecap="round"/>')

def i_diamond(k, fill="#fff"):   # built-in: thunderbolt glyph (rotated square)
    s = 0.62*k
    return f'<rect x="{-s:.1f}" y="{-s:.1f}" width="{2*s:.1f}" height="{2*s:.1f}" rx="{0.18*k:.1f}" transform="rotate(45)" fill="{fill}" stroke="{INK}" stroke-width="2.4"/>'

# ---------- central hub ----------
island(0, 0, 1.7, TAN)
S.box(-1.35, -1.35, 0, 2.7, 2.7, 1.7, PURPLE,
      top=shade(PURPLE, 1.18), left=PURPLE, right=PURPLE_D)
hx, hy, hd = S.top_center(-1.35, -1.35, 0, 2.7, 2.7, 1.7)
S.icon(hx, hy - 2, hd, i_bolt(30, "#fff"))
# hub identity sits in the empty bottom-center gap
S.tag(2.55, 2.55, 0.0, "Thunderbolt", bg=PURPLE, fg="#fff", dot=YELLOW)
S.tag(2.55, 2.55, 0.0, "your context home", bg="#fff", fg=INK, mono=True, dy=30)

def acp_pill(dx, dy):
    """Tiny ink 'acp' pill on a connector midpoint — every backend speaks ACP."""
    px, py = S.p(dx*0.585, dy*0.585, 0.5)
    w, h = 46, 22
    S.raw(f'<g transform="translate({px:.0f} {py:.0f})">'
          f'<rect x="{-w/2}" y="{-h/2}" width="{w}" height="{h}" rx="11" '
          f'fill="{INK}" stroke="{INK}" stroke-width="2"/>'
          f'<text x="0" y="4" text-anchor="middle" font-family="Space Mono, monospace" '
          f'font-weight="700" font-size="13" letter-spacing="1" fill="{CREAM}">ACP</text></g>',
          1e6)

# ---------- backends ----------
DIST = 5.9
backends = [
    ( DIST,  0.0, GREEN,  i_term,    "Claude Code", GREEN,  "below"),
    ( 0.0,  DIST, PINK,   i_stack,   "Haystack",    PINK,   "below"),
    (-DIST,  0.0, YELLOW, i_wing,    "Hermes",      YELLOW, "above"),
    ( 0.0, -DIST, PURPLE, i_diamond, "Built-in",    PURPLE, "above"),
]
for dx, dy, color, icon_fn, name, dot, side in backends:
    island(dx, dy, 1.05, TAN)
    S.box(dx-0.8, dy-0.8, 0, 1.6, 1.6, 1.05, color,
          top=shade(color, 1.18), left=shade(color, 0.82), right=shade(color, 0.6))
    cx, cy, cd = S.top_center(dx-0.8, dy-0.8, 0, 1.6, 1.6, 1.05)
    S.icon(cx, cy - 2, cd, icon_fn(22, INK if color == YELLOW else "#fff"))
    # ACP connector: hub edge -> backend edge, riding tokens in backend color
    S.dpath((dx*0.23, dy*0.23, 0.5), (dx*0.52, dy*0.52, 0.5), (dx*0.79, dy*0.79, 0.5),
            color=INK, tokens=3, token_color=color, dur=2.8)
    acp_pill(dx, dy)
    S.tag(dx, dy, 1.05, name, bg="#fff", fg=INK, dot=dot,
          dy=(-52 if side == "above" else 34))

# ---------- bring-your-own card (floats above hub, plugs in) ----------
ax, ay = S.p(0, 0, 1.7)          # hub top-center anchor
card_cx, card_cy = ax, ay - 168
cw, ch = 240, 78
depth = 1e6
# connector card -> hub (screen-space dotted drop with a token)
S.raw(f'<path id="byo" d="M {card_cx:.0f} {card_cy+ch/2:.0f} L {ax:.0f} {ay-6:.0f}" '
      f'fill="none" stroke="{INK}" stroke-width="3.5" stroke-linecap="round" stroke-dasharray="1 9"/>'
      f'<circle r="5.5" fill="{PURPLE}" stroke="{INK}" stroke-width="2.5">'
      f'<animateMotion dur="1.9s" repeatCount="indefinite" keyPoints="0;1" keyTimes="0;1" '
      f'calcMode="linear"><mpath href="#byo"/></animateMotion></circle>', depth)
chip_cols = [GREEN, YELLOW, PINK]
chips = "".join(
    f'<rect x="{-56 + i*24:.0f}" y="12" width="18" height="18" rx="5" '
    f'fill="{c}" stroke="{INK}" stroke-width="2.4"/>' for i, c in enumerate(chip_cols))
S.raw(
    f'<g transform="translate({card_cx:.0f} {card_cy:.0f})">'
    f'<rect x="{-cw/2:.0f}" y="{-ch/2:.0f}" width="{cw}" height="{ch}" rx="18" '
    f'fill="#fff" stroke="{INK}" stroke-width="3" filter="none" '
    f'style="filter:drop-shadow(5px 5px 0 {INK})"/>'
    f'<text x="0" y="-14" text-anchor="middle" font-family="Space Mono, monospace" '
    f'font-size="11" letter-spacing="1.5" fill="#8a7f9c">BRING YOUR OWN</text>'
    f'<text x="0" y="7" text-anchor="middle" font-family="Space Grotesk, sans-serif" '
    f'font-weight="700" font-size="20" fill="{INK}">skills + MCPs</text>'
    f'{chips}'
    f'<text x="{-56 + 3*24 + 2:.0f}" y="26" font-family="Space Mono, monospace" '
    f'font-size="12" fill="#6b6478">plug in →</text>'
    f'</g>', depth + 1)

# ---------- fonts (self-contained) ----------
def fonts_css():
    d = os.path.join(os.path.dirname(__file__), "..", "..", "..", "..",
                     "website", "src", "fonts")
    faces = [("Space Grotesk", "space-grotesk-latin.woff2", "400 700"),
             ("Space Mono", "space-mono-400-latin.woff2", "400"),
             ("Space Mono", "space-mono-700-latin.woff2", "700")]
    css = ""
    for fam, fn, wt in faces:
        p = os.path.join(d, fn)
        if not os.path.exists(p):
            return ""
        b64 = base64.b64encode(open(p, "rb").read()).decode()
        css += (f"@font-face{{font-family:'{fam}';font-weight:{wt};font-display:swap;"
                f"src:url(data:font/woff2;base64,{b64}) format('woff2');}}")
    return css

use_fonts = "--no-fonts" not in sys.argv
print(S.render(W, H, bg=CREAM, fonts_css=fonts_css() if use_fonts else ""))
