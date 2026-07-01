#!/usr/bin/env python3
"""
iso.py — a tiny, dependency-free isometric SVG toolkit.

This is the in-environment engine for the `isometric-graphic` skill: it produces
crisp, editable, brand-consistent isometric vector art without any external
account or model. Everything is plain SVG, so the output stays razor-sharp,
recolorable, and animatable (path-trim / dashoffset) — the same properties the
Recraft -> Figma -> Lottie pipeline is chasing, but deterministic and reviewable.

Projection: true 30 degree axonometric.
    +x -> down-right,  +y -> down-left,  +z -> up.
    The camera sees the TOP face plus the two front vertical faces (left/right).

Core helpers:
    S = Scene(unit=..., origin=(ox,oy))
    S.box(x, y, z, w, d, h, color)         -> shaded cuboid (top/left/right)
    S.dpath((x0,y0,z0), (x1,y1,z1), ...)   -> isometric dotted connector
    S.token(...)                            -> a marker that rides a connector
    S.tag(x, y, z, text, ...)              -> a flat pop-in label chip
    S.raw(svg_string)                       -> escape hatch for bespoke shapes
    print(S.render(width, height))          -> final <svg> document

Colors auto-derive top/left/right shading from a single base color, so a scene
recolors to a new palette by changing base colors only.
"""
from __future__ import annotations
import math

COS30 = math.cos(math.radians(30))
SIN30 = math.sin(math.radians(30))  # 0.5


# ----------------------------------------------------------------------------- color
def _hex(c: str) -> tuple[int, int, int]:
    c = c.lstrip("#")
    return int(c[0:2], 16), int(c[2:4], 16), int(c[4:6], 16)


def _rgb(t: tuple[int, int, int]) -> str:
    return "#%02x%02x%02x" % tuple(max(0, min(255, int(round(v)))) for v in t)


def shade(color: str, factor: float) -> str:
    """factor >1 lightens toward white, <1 darkens toward black."""
    r, g, b = _hex(color)
    if factor >= 1:
        t = factor - 1
        return _rgb((r + (255 - r) * t, g + (255 - g) * t, b + (255 - b) * t))
    return _rgb((r * factor, g * factor, b * factor))


# ----------------------------------------------------------------------------- scene
class Scene:
    def __init__(self, unit: float = 28.0, origin: tuple[float, float] = (0, 0),
                 ink: str = "#221d2c", stroke: float = 2.5):
        self.unit = unit
        self.ox, self.oy = origin
        self.ink = ink
        self.stroke = stroke
        self._layers: list[tuple[float, str]] = []  # (depth, svg) painter's algorithm

    # -- projection --
    def p(self, x: float, y: float, z: float = 0.0) -> tuple[float, float]:
        sx = self.ox + (x - y) * COS30 * self.unit
        sy = self.oy + (x + y) * SIN30 * self.unit - z * self.unit
        return sx, sy

    @staticmethod
    def _depth(x: float, y: float, z: float) -> float:
        # larger x+y is nearer the camera; higher z is nearer. Bigger => draw later.
        return (x + y) * 2 + z

    def add(self, depth: float, svg: str):
        self._layers.append((depth, svg))

    def raw(self, svg: str, depth: float = 1e6):
        self.add(depth, svg)

    # -- primitives --
    def _poly(self, pts, fill, stroke=None, sw=None, extra=""):
        sw = self.stroke if sw is None else sw
        stroke = self.ink if stroke is None else stroke
        d = " ".join(f"{px:.2f},{py:.2f}" for px, py in pts)
        return (f'<polygon points="{d}" fill="{fill}" stroke="{stroke}" '
                f'stroke-width="{sw}" stroke-linejoin="round" {extra}/>')

    def box(self, x, y, z, w, d, h, color, *, top=None, left=None, right=None,
            label=None, label_color="#fff", sub=None, glyph=None):
        """Draw a shaded cuboid. Footprint w (+x) x d (+y), height h, base at z."""
        top = top or shade(color, 1.14)
        left = left or shade(color, 0.82)
        right = right or shade(color, 0.62)
        P = self.p
        # corners
        t = [P(x, y, z + h), P(x + w, y, z + h), P(x + w, y + d, z + h), P(x, y + d, z + h)]
        # left face  (constant x = x+w) : appears on screen-left
        lf = [P(x + w, y, z), P(x + w, y + d, z), P(x + w, y + d, z + h), P(x + w, y, z + h)]
        # right face (constant y = y+d) : appears on screen-right
        rf = [P(x, y + d, z), P(x + w, y + d, z), P(x + w, y + d, z + h), P(x, y + d, z + h)]
        dep = self._depth(x + w, y + d, z + h)
        svg = [self._poly(rf, right), self._poly(lf, left), self._poly(t, top)]
        # top-face glyph / label centered on top
        cx, cy = P(x + w / 2, y + d / 2, z + h)
        if glyph:
            svg.append(f'<text x="{cx:.1f}" y="{cy+glyph_dy(glyph):.1f}" text-anchor="middle" '
                       f'font-family="Space Grotesk, sans-serif" font-weight="800" '
                       f'font-size="{self.unit*0.9:.0f}" fill="{label_color}">{glyph}</text>')
        self.add(dep, "".join(svg))
        if label:
            self.face_label(x, y, z, w, d, h, label, label_color, sub)

    def face_label(self, x, y, z, w, d, h, label, color="#fff", sub=None):
        """Text laid on the left front face of a box (upright, readable)."""
        px, py = self.p(x + w, y + d * 0.5, z + h * 0.5)
        dep = self._depth(x + w, y + d, z + h) + 0.5
        fs = self.unit * 0.42
        t = (f'<text x="{px:.1f}" y="{py:.1f}" text-anchor="middle" '
             f'font-family="Space Grotesk, sans-serif" font-weight="700" '
             f'font-size="{fs:.0f}" fill="{color}">{label}</text>')
        if sub:
            t += (f'<text x="{px:.1f}" y="{py+fs*1.05:.1f}" text-anchor="middle" '
                  f'font-family="Space Mono, monospace" font-weight="400" '
                  f'font-size="{fs*0.62:.0f}" fill="{color}" opacity="0.72">{sub}</text>')
        self.add(dep, t)

    def dpath(self, *points, color=None, width=None, dash="1 9", tokens=0,
              token_color="#ffce3a", animate=True, dur=2.4, depth=None):
        """Dotted isometric connector through 3D points, with optional riding tokens."""
        color = color or self.ink
        width = width if width is not None else self.stroke + 1
        pts = [self.p(*pt) for pt in points]
        d = "M " + " L ".join(f"{px:.2f} {py:.2f}" for px, py in pts)
        dep = depth if depth is not None else max(self._depth(*pt) for pt in points) - 0.4
        pid = f"p{len(self._layers)}"
        svg = [f'<path id="{pid}" d="{d}" fill="none" stroke="{color}" '
               f'stroke-width="{width}" stroke-linecap="round" stroke-dasharray="{dash}"/>']
        for i in range(tokens):
            off = i / max(1, tokens)
            begin = f'{-off*dur:.2f}s'
            anim = (f'<animateMotion dur="{dur}s" repeatCount="indefinite" begin="{begin}" '
                    f'keyPoints="0;1" keyTimes="0;1" calcMode="linear">'
                    f'<mpath href="#{pid}"/></animateMotion>') if animate else ""
            svg.append(f'<circle r="{width*1.7:.1f}" fill="{token_color}" '
                       f'stroke="{self.ink}" stroke-width="{self.stroke}">{anim}</circle>')
        self.add(dep, "".join(svg))

    def tag(self, x, y, z, text, *, bg="#fff", fg=None, dot=None, depth=None,
            dy=0.0, mono=False):
        """A flat pop-in chip label floating at a 3D anchor (screen-upright)."""
        fg = fg or self.ink
        px, py = self.p(x, y, z)
        py += dy
        fam = "Space Mono, monospace" if mono else "Space Grotesk, sans-serif"
        fs = self.unit * 0.44
        pad = fs * 0.8
        w = len(text) * fs * (0.62 if mono else 0.56) + pad * 2 + (fs if dot else 0)
        h = fs * 1.9
        dep = depth if depth is not None else self._depth(x, y, z) + 5
        rx = h / 2
        parts = [f'<rect x="{px-w/2:.1f}" y="{py-h/2:.1f}" width="{w:.1f}" height="{h:.1f}" '
                 f'rx="{rx:.1f}" fill="{bg}" stroke="{self.ink}" stroke-width="{self.stroke}"/>']
        tx = px - w / 2 + pad
        if dot:
            parts.append(f'<circle cx="{tx+fs*0.35:.1f}" cy="{py:.1f}" r="{fs*0.35:.1f}" '
                         f'fill="{dot}" stroke="{self.ink}" stroke-width="1.5"/>')
            tx += fs
        parts.append(f'<text x="{tx:.1f}" y="{py+fs*0.34:.1f}" '
                     f'font-family="{fam}" font-weight="700" font-size="{fs:.0f}" '
                     f'fill="{fg}">{text}</text>')
        self.add(dep, "".join(parts))

    def top_center(self, x, y, z, w, d, h):
        """Screen coords of a box's top-face center + a depth that sits above it."""
        cx, cy = self.p(x + w / 2, y + d / 2, z + h)
        return cx, cy, self._depth(x + w, y + d, z + h) + 0.3

    def icon(self, cx, cy, depth, svg):
        """Place a screen-space icon group centered at (cx,cy)."""
        self.add(depth, f'<g transform="translate({cx:.1f} {cy:.1f})">{svg}</g>')

    def render(self, width: int, height: int, bg: str = "#f7f1e6",
               fonts_css: str = "") -> str:
        body = "".join(s for _, s in sorted(self._layers, key=lambda t: t[0]))
        style = f"<style>{fonts_css}</style>" if fonts_css else ""
        return (f'<svg xmlns="http://www.w3.org/2000/svg" '
                f'xmlns:xlink="http://www.w3.org/1999/xlink" '
                f'width="{width}" height="{height}" viewBox="0 0 {width} {height}">'
                f'{style}<rect width="{width}" height="{height}" fill="{bg}"/>'
                f'{body}</svg>')


def glyph_dy(_g: str) -> float:
    return 10.0  # vertical nudge to visually center a glyph on a top face


if __name__ == "__main__":
    # smoke test: three boxes + a connector
    S = Scene(unit=30, origin=(400, 120))
    S.box(0, 0, 0, 2, 2, 1.4, "#6b4ef0", label="HUB")
    S.box(4, 0, 0, 1.4, 1.4, 1.0, "#35c07a", label="A")
    S.box(0, 4, 0, 1.4, 1.4, 1.0, "#ff7eb6", label="B")
    S.dpath((2, 1, 0.7), (4, 0.7, 0.7), tokens=2)
    print(S.render(800, 520))
