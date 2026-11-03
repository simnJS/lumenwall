#!/usr/bin/env python3
"""Lumenwall brand assets: logo, app icon, tray icons, README banner, brand sheet.

Every SVG under assets/brand/ is written by this script from the geometry below, so a
change of proportion or colour is made once here (see docs/BRAND.md for the rules).

    python3 assets/brand/build.py                 # SVG + PNG + ICO + Linux icon theme
    python3 assets/brand/build.py --no-packaging  # same, without writing packaging/linux/icons
    python3 assets/brand/build.py --svg-only      # SVG sources only (no external tool)

Rasterizing needs `resvg` and `usvg` 0.45 on the PATH (`cargo install resvg usvg`), or
`--resvg PATH --usvg PATH`. `usvg` converts the banner and sheet text to outlines with
the Lato font (OFL-1.1, packaged by most Linux distributions as fonts-lato / lato-fonts),
so the published SVGs do not depend on installed fonts. Only the Python standard library
is used; the ICO file is written by hand (PNG-compressed entries, Windows Vista and later).
"""

import argparse
import math
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent

# ------------------------------------------------------------------ palette (docs/BRAND.md)

ID = "lumenwall"  # technical identifier: file, icon and package names
NAME = "Lumenwall"

AMBER = "#ffb347"  # brand accent (dark theme accent token)
AMBER_HI = "#ffc46e"  # accent hover, horizon highlight
DAWN = "#ffe2ad"  # top of the sun gradient
EMBER = "#ff8a3d"  # bottom of the sun gradient
EMBER_DEEP = "#e8711f"  # sun bottom on light backgrounds
NIGHT = "#111217"  # dark background
INK = "#16171c"  # light-theme text, mono logo
MIST = "#eeeff4"  # dark-theme text
MUTED = "#a3a7b7"  # dark-theme secondary text
DUSK_TOP = "#282a44"  # app icon sky, top
DUSK_LOW = "#55301f"  # app icon sky at the horizon
SEA = "#161722"  # app icon, below the horizon

# ------------------------------------------------------------------ geometry helpers


def n(v):
    s = f"{v:.3f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def seg(x1, y1, x2, y2, w):
    """Round-capped line as a filled outline (strokes are outlined for symbolic icons)."""
    h = w / 2
    dx, dy = x2 - x1, y2 - y1
    length = math.hypot(dx, dy)
    nx, ny = -dy / length * h, dx / length * h
    return (
        f"M{n(x1 + nx)} {n(y1 + ny)}L{n(x2 + nx)} {n(y2 + ny)}"
        f"A{n(h)} {n(h)} 0 0 0 {n(x2 - nx)} {n(y2 - ny)}L{n(x1 - nx)} {n(y1 - ny)}"
        f"A{n(h)} {n(h)} 0 0 0 {n(x1 + nx)} {n(y1 + ny)}Z"
    )


def arc(cx, cy, r, a0, a1, w):
    """Round-capped arc, centre radius r, from angle a0 to a1 (degrees, y down)."""
    h = w / 2
    large = 1 if a1 - a0 > 180 else 0

    def p(a, rr):
        return cx + rr * math.cos(math.radians(a)), cy + rr * math.sin(math.radians(a))

    o0, o1, i1, i0 = p(a0, r + h), p(a1, r + h), p(a1, r - h), p(a0, r - h)
    return (
        f"M{n(o0[0])} {n(o0[1])}A{n(r + h)} {n(r + h)} 0 {large} 1 {n(o1[0])} {n(o1[1])}"
        f"A{n(h)} {n(h)} 0 0 1 {n(i1[0])} {n(i1[1])}"
        f"A{n(r - h)} {n(r - h)} 0 {large} 0 {n(i0[0])} {n(i0[1])}"
        f"A{n(h)} {n(h)} 0 0 1 {n(o0[0])} {n(o0[1])}Z"
    )


def sun(cx, cy, r, cut):
    """Disc whose part below y = cut is removed: the sun touching the horizon."""
    dy = cut - cy
    hw = math.sqrt(r * r - dy * dy)
    large = 1 if dy > 0 else 0
    return f"M{n(cx - hw)} {n(cut)}A{n(r)} {n(r)} 0 {large} 1 {n(cx + hw)} {n(cut)}Z"


def sun_ring(cx, cy, r, cut, w):
    """Outline of `sun` (paused state), as one even-odd path."""
    return sun(cx, cy, r, cut) + sun(cx, cy, r - w, cut - w)


# ------------------------------------------------------------------ symbol
# 24-unit grid, same grid and 2-unit weight as the UI icons (crates/lumen-ui/src/icons.rs).
# Sun: centre (12, 10.75), r 5.75, cut at y 12.5. Horizon: y 15, x 2.5..21.5.
# Ripple: y 18.5, x 7.5..16.5. Gaps between the three: 1.5 units. Bounding box 1.5..22.5 x 5..19.5.

SYMBOL_SUN = (12, 10.75, 5.75, 12.5)
SYMBOL_HORIZON = (2.5, 15, 21.5, 15, 2)
SYMBOL_RIPPLE = (7.5, 18.5, 16.5, 18.5, 2)
SYMBOL_BOX = (1.5, 5, 21, 14.5)  # x, y, w, h of the ink


def symbol_paths(fill_sun, fill_lines, dx=0.0, dy=0.0, s=1.0):
    cx, cy, r, cut = SYMBOL_SUN

    def t(v, o):
        return v * s + o

    x1, y1, x2, y2, w = SYMBOL_HORIZON
    a1, b1, a2, b2, w2 = SYMBOL_RIPPLE
    return (
        f'<path fill="{fill_sun}" d="{sun(t(cx, dx), t(cy, dy), r * s, t(cut, dy))}"/>'
        f'<path fill="{fill_lines}" d="{seg(t(x1, dx), t(y1, dy), t(x2, dx), t(y2, dy), w * s)}"/>'
        f'<path fill="{fill_lines}" d="{seg(t(a1, dx), t(b1, dy), t(a2, dx), t(b2, dy), w2 * s)}"/>'
    )


def sun_gradient(gid, y0, y1, bottom=EMBER):
    """Dawn -> amber -> ember from the top of the sun to its chord. On light backgrounds
    (bottom = EMBER_DEEP) the pale dawn top would vanish, so the gradient starts at amber."""
    top = AMBER if bottom == EMBER_DEEP else DAWN
    mid = "#ff9b3a" if bottom == EMBER_DEEP else AMBER
    return (
        f'<linearGradient id="{gid}" x1="0" y1="{n(y0)}" x2="0" y2="{n(y1)}" '
        f'gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="{top}"/>'
        f'<stop offset="0.55" stop-color="{mid}"/><stop offset="1" stop-color="{bottom}"/>'
        "</linearGradient>"
    )


# ------------------------------------------------------------------ wordmark
# Lowercase "lumenwall", monoline with round terminals, drawn (not typeset): no font licence.
# Units: stroke 2.6, x-height 12.5, ascender 16.5, baseline y = 0. Each glyph is built from
# round-capped segments and arcs of the same weight; strokes are centred so that every
# terminal and vertex lands exactly on the baseline, x-height or ascender.

WORD = "lumenwall"
WM_W, WM_X, WM_A = 2.6, 12.5, 16.5
WM_DN, WM_DM = 8.6, 7.4  # stem distance of n/u and of each m arch
WM_WW, WM_APEX = 14.4, 0.85  # w: width between outer stroke centres, height of its middle apex
# Spacing between ink edges, by the shape of the facing sides: S straight stem, R round
# bowl, D diagonal. Diagonals and bowls leave open space at the edge, so they sit closer.
WM_GAPS = {"SS": 3.3, "SR": 2.7, "RS": 2.7, "RR": 2.2, "SD": 1.3, "DS": 1.3, "RD": 0.9, "DR": 0.9, "DD": 0.8}
WM_KERN = {"lu": 3.0, "ll": 3.0}  # a lone ascender stem needs a little less room


def wordmark_glyphs():
    """Glyph -> (draw(x) -> parts, width between outer stroke centres, left side, right side)."""
    w, h = WM_W, WM_W / 2
    top, bot = -WM_X + h, -h
    r, rm = WM_DN / 2, WM_DM / 2
    re = (WM_X - w) / 2  # bowl radius of e and a
    q = WM_WW / 4
    apex = top + (bot - top) * (1 - WM_APEX)
    return {
        "l": (lambda x: [seg(x, -WM_A + h, x, bot, w)], 0, "S", "S"),
        "u": (
            lambda x: [seg(x, top, x, bot - r, w), arc(x + r, bot - r, r, 0, 180, w), seg(x + WM_DN, top, x + WM_DN, bot, w)],
            WM_DN, "S", "S",
        ),
        "m": (
            lambda x: [
                seg(x, top, x, bot, w),
                arc(x + rm, top + rm, rm, -180, 0, w),
                seg(x + WM_DM, top + rm, x + WM_DM, bot, w),
                arc(x + WM_DM + rm, top + rm, rm, -180, 0, w),
                seg(x + 2 * WM_DM, top + rm, x + 2 * WM_DM, bot, w),
            ],
            2 * WM_DM, "S", "S",
        ),
        # e: circle open 40 degrees below the bar, bar across the full bowl.
        "e": (lambda x: [seg(x, -WM_X / 2, x + 2 * re, -WM_X / 2, w), arc(x + re, -WM_X / 2, re, -320, 0, w)], 2 * re, "R", "R"),
        "n": (
            lambda x: [seg(x, top, x, bot, w), arc(x + r, top + r, r, -180, 0, w), seg(x + WM_DN, top + r, x + WM_DN, bot, w)],
            WM_DN, "S", "S",
        ),
        # w: four diagonals with round vertices; the middle apex stops at 85 % of the x-height
        # so the centre of the word does not darken.
        "w": (
            lambda x: [
                seg(x, top, x + q, bot, w),
                seg(x + q, bot, x + 2 * q, apex, w),
                seg(x + 2 * q, apex, x + 3 * q, bot, w),
                seg(x + 3 * q, bot, x + 4 * q, top, w),
            ],
            WM_WW, "D", "D",
        ),
        # a: single-storey, the e's circle closed against a full-height stem.
        "a": (
            lambda x: [arc(x + re, -WM_X / 2, re, -180, 0, w), arc(x + re, -WM_X / 2, re, 0, 180, w), seg(x + 2 * re, top, x + 2 * re, bot, w)],
            2 * re, "R", "S",
        ),
    }


def wordmark_parts(word=WORD):
    glyphs = wordmark_glyphs()
    h = WM_W / 2
    x, parts, prev = h, [], None
    for c in word:
        draw, width, left, right = glyphs[c]
        if prev:
            pc, pr = prev
            x += h + WM_KERN.get(pc + c, WM_GAPS[pr + left]) + h
        parts += draw(x)
        x += width
        prev = (c, right)
    return parts, x + h


WM_PARTS, WM_WIDTH = wordmark_parts()


def wordmark_g(fill, x, baseline, scale):
    # One <path> per stroke: overlapping sub-paths of one path would cancel at the joins.
    paths = "".join(f'<path d="{d}"/>' for d in WM_PARTS)
    return f'<g fill="{fill}" transform="translate({n(x)} {n(baseline)}) scale({n(scale)})">{paths}</g>'


# ------------------------------------------------------------------ SVG documents


def svg_doc(w, h, body, title, view=None):
    vb = view or f"0 0 {n(w)} {n(h)}"
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{n(w)}" height="{n(h)}" viewBox="{vb}">'
        f"<title>{title}</title>{body}</svg>\n"
    )


def symbol_svg(variant):
    title = f"{NAME} symbol"
    if variant == "color":
        body = f"<defs>{sun_gradient('sun', 5, 12.5)}</defs>" + symbol_paths("url(#sun)", AMBER)
    elif variant == "color-light":
        body = f"<defs>{sun_gradient('sun', 5, 12.5, EMBER_DEEP)}</defs>" + symbol_paths("url(#sun)", EMBER_DEEP)
    else:
        c = {"mono": INK, "inverse": "#ffffff"}[variant]
        body = symbol_paths(c, c)
    return svg_doc(24, 24, body, title)


# Horizontal lockup: symbol height = 1.3 x wordmark ascender, centred on the ascender band,
# gap = 0.42 x symbol height. Clear space around the lockup = x-height of the wordmark.


def lockup(sym_sun, sym_lines, word_fill, defs="", gid="sun"):
    s = (WM_A * 1.3) / SYMBOL_BOX[3]  # symbol scale
    sym_w, sym_h = SYMBOL_BOX[2] * s, SYMBOL_BOX[3] * s
    gap = 0.42 * sym_h
    pad = WM_X  # clear space
    word_x = pad + sym_w + gap
    baseline = pad + (sym_h + WM_A) / 2
    sym_dx = pad - SYMBOL_BOX[0] * s
    sym_dy = baseline - WM_A / 2 - sym_h / 2 - SYMBOL_BOX[1] * s
    width = word_x + WM_WIDTH + pad
    height = sym_h + 2 * pad
    sun_fill = sym_sun
    if defs:
        cx, cy, r, cut = SYMBOL_SUN
        defs = f"<defs>{sun_gradient(gid, (cy - r) * s + sym_dy, cut * s + sym_dy, defs)}</defs>"
        sun_fill = f"url(#{gid})"
    body = defs + symbol_paths(sun_fill, sym_lines, sym_dx, sym_dy, s) + wordmark_g(word_fill, word_x, baseline, 1)
    return width, height, body


def placed_lockup(x, y, scale, word_fill, line, bottom, gid):
    """The horizontal lockup with its ink box's top-left corner at (x, y), for compositions."""
    _, _, body = lockup(None, line, word_fill, defs=bottom, gid=gid)
    pad = WM_X
    return f'<g transform="translate({n(x - pad * scale)} {n(y - pad * scale)}) scale({n(scale)})">{body}</g>'


def lockup_svg(variant):
    if variant == "on-dark":
        w, h, body = lockup(None, AMBER, MIST, defs=EMBER)
    elif variant == "on-light":
        w, h, body = lockup(None, EMBER_DEEP, INK, defs=EMBER_DEEP)
    else:
        c = {"mono": INK, "inverse": "#ffffff"}[variant]
        w, h, body = lockup(c, c, c)
    return svg_doc(w, h, body, NAME)


def stacked_svg(variant):
    word_fill, line, bottom = (MIST, AMBER, EMBER) if variant == "on-dark" else (INK, EMBER_DEEP, EMBER_DEEP)
    s = 2.6  # symbol about half the wordmark's width
    pad = WM_X
    sym_w, sym_h = SYMBOL_BOX[2] * s, SYMBOL_BOX[3] * s
    width = max(sym_w, WM_WIDTH) + 2 * pad
    sym_dx = (width - sym_w) / 2 - SYMBOL_BOX[0] * s
    sym_dy = pad - SYMBOL_BOX[1] * s
    baseline = pad + sym_h + 0.75 * WM_X + WM_A
    height = baseline + pad
    cx, cy, r, cut = SYMBOL_SUN
    defs = f"<defs>{sun_gradient('sun', (cy - r) * s + sym_dy, cut * s + sym_dy, bottom)}</defs>"
    body = (
        defs
        + symbol_paths("url(#sun)", line, sym_dx, sym_dy, s)
        + wordmark_g(word_fill, (width - WM_WIDTH) / 2, baseline, 1)
    )
    return svg_doc(width, height, body, NAME)


def wordmark_svg(fill):
    pad = 2
    w, h = WM_WIDTH + 2 * pad, WM_A + 2 * pad
    return svg_doc(w, h, wordmark_g(fill, pad, pad + WM_A, 1), NAME)


# ------------------------------------------------------------------ app icon


def app_icon(size, tile, rx, sunp, horizon, ripples, glow_r, rim):
    """One app-icon master. tile = (x, y, side); sunp = (cx, cy, r, cut);
    horizon = (y0, y1) band clipped by the tile; ripples = [(x0, x1, y, w, opacity)]."""
    tx, ty, tw = tile
    cx, cy, r, cut = sunp
    hy0, hy1 = horizon
    out = [
        "<defs>",
        f'<clipPath id="tile"><rect x="{n(tx)}" y="{n(ty)}" width="{n(tw)}" height="{n(tw)}" rx="{n(rx)}"/></clipPath>',
        f'<linearGradient id="sky" x1="0" y1="{n(ty)}" x2="0" y2="{n(hy0)}" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0" stop-color="{DUSK_TOP}"/><stop offset="0.55" stop-color="#2f2939"/>'
        f'<stop offset="1" stop-color="{DUSK_LOW}"/></linearGradient>',
        f'<linearGradient id="sea" x1="0" y1="{n(hy1)}" x2="0" y2="{n(ty + tw)}" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0" stop-color="#1d1a22"/><stop offset="1" stop-color="{SEA}"/></linearGradient>',
        f'<radialGradient id="glow" cx="{n(cx)}" cy="{n(cut)}" r="{n(glow_r)}" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0" stop-color="{AMBER}" stop-opacity="0.5"/><stop offset="0.45" stop-color="{EMBER}" '
        f'stop-opacity="0.16"/><stop offset="1" stop-color="{EMBER}" stop-opacity="0"/></radialGradient>',
        sun_gradient("sun", cy - r, cut),
        f'<linearGradient id="hz" x1="{n(tx)}" y1="0" x2="{n(tx + tw)}" y2="0" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0" stop-color="{AMBER}" stop-opacity="0.55"/><stop offset="0.5" stop-color="{AMBER_HI}"/>'
        f'<stop offset="1" stop-color="{AMBER}" stop-opacity="0.55"/></linearGradient>',
        "</defs>",
        '<g clip-path="url(#tile)">',
        f'<rect x="{n(tx)}" y="{n(ty)}" width="{n(tw)}" height="{n(hy0 - ty)}" fill="url(#sky)"/>',
        f'<rect x="{n(tx)}" y="{n(hy0)}" width="{n(tw)}" height="{n(ty + tw - hy0)}" fill="url(#sea)"/>',
        f'<rect x="{n(tx)}" y="{n(ty)}" width="{n(tw)}" height="{n(tw)}" fill="url(#glow)"/>',
        f'<path fill="url(#sun)" d="{sun(cx, cy, r, cut)}"/>',
        f'<rect x="{n(tx)}" y="{n(hy0)}" width="{n(tw)}" height="{n(hy1 - hy0)}" fill="url(#hz)"/>',
    ]
    for x0, x1, y, w, op in ripples:
        out.append(f'<path fill="{AMBER}" fill-opacity="{op}" d="{seg(x0, y, x1, y, w)}"/>')
    out.append("</g>")
    if rim:
        out.append(
            f'<rect x="{n(tx + rim / 2)}" y="{n(ty + rim / 2)}" width="{n(tw - rim)}" height="{n(tw - rim)}" '
            f'rx="{n(rx - rim / 2)}" fill="none" stroke="#ffffff" stroke-opacity="0.08" stroke-width="{n(rim)}"/>'
        )
    return svg_doc(size, size, "".join(out), NAME)


# Masters: 1024 (64 px and up, scalable), 32 (32 and 48 px), 24, 16. Small masters are
# pixel-snapped: horizon and ripples on whole pixels, one ripple only.
ICON_MASTERS = {
    1024: dict(
        tile=(64, 64, 896), rx=200, sunp=(512, 470, 228, 552), horizon=(574, 614),
        ripples=[(352, 672, 676, 40, 0.85), (432, 592, 750, 34, 0.5)], glow_r=470, rim=2,
    ),
    32: dict(
        tile=(1, 1, 30), rx=7, sunp=(16, 13.5, 8, 16.5), horizon=(18, 20.5),
        ripples=[(11, 21, 23.75, 2, 0.85), (13.5, 18.5, 27, 1.75, 0.5)], glow_r=15, rim=0,
    ),
    24: dict(
        tile=(1, 1, 22), rx=5, sunp=(12, 10.5, 6, 12.5), horizon=(14, 16),
        ripples=[(8.5, 15.5, 18.5, 1.5, 0.85)], glow_r=11, rim=0,
    ),
    16: dict(
        tile=(0, 0, 16), rx=3.5, sunp=(8, 7, 4.25, 8), horizon=(9, 11),
        ripples=[(5.5, 10.5, 12.5, 1, 0.9)], glow_r=8, rim=0,
    ),
}
PNG_SIZES = {16: 16, 24: 24, 32: 32, 48: 32, 64: 1024, 128: 1024, 256: 1024, 512: 1024, 1024: 1024}
HICOLOR = [16, 24, 32, 48, 64, 128, 256, 512]
ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]

# ------------------------------------------------------------------ tray (symbolic)
# 16-unit grid, pixel-snapped: sun top 2.5, chord 8, horizon rows 9-10, ripple rows 12-13.

TRAY_SUN = (8, 6.75, 4.25, 8)


def tray_svg(paused, fill="#2e3436"):
    cx, cy, r, cut = TRAY_SUN
    sun_d = sun_ring(cx, cy, r, cut, 1.5) if paused else sun(cx, cy, r, cut)
    lines = seg(2, 10, 14, 10, 2) + seg(6, 13, 10, 13, 2)
    title = f"{NAME} (paused)" if paused else NAME
    body = (
        f'<style id="current-color-scheme">.ColorScheme-Text{{color:{fill}}}</style>'
        # fill attribute for strict symbolic parsers (GTK), style for KDE's colour scheme.
        f'<path class="ColorScheme-Text" fill="{fill}" style="fill:currentColor" fill-rule="evenodd" d="{sun_d}"/>'
        f'<path class="ColorScheme-Text" fill="{fill}" style="fill:currentColor" d="{lines}"/>'
    )
    return svg_doc(16, 16, body, title)


def tray_flat(paused, fill, grid):
    """Flat glyph for the Windows tray PNGs: 16 grid (16, 20, 32 px) or the 24-grid symbol (24 px)."""
    if grid == 24:
        cx, cy, r, cut = SYMBOL_SUN
        sun_d = sun_ring(cx, cy, r, cut, 2) if paused else sun(cx, cy, r, cut)
        x1, y1, x2, y2, w = SYMBOL_HORIZON
        a1, b1, a2, b2, w2 = SYMBOL_RIPPLE
        lines = seg(x1, y1, x2, y2, w) + seg(a1, b1, a2, b2, w2)
    else:
        cx, cy, r, cut = TRAY_SUN
        sun_d = sun_ring(cx, cy, r, cut, 1.5) if paused else sun(cx, cy, r, cut)
        lines = seg(2, 10, 14, 10, 2) + seg(6, 13, 10, 13, 2)
    body = f'<path fill="{fill}" fill-rule="evenodd" d="{sun_d}"/><path fill="{fill}" d="{lines}"/>'
    return svg_doc(grid, grid, body, NAME)


# ------------------------------------------------------------------ banner (1280 x 640)

BANNER_TEXT = {
    "en": dict(
        tagline=["Animated wallpapers that sleep", "when you can’t see them."],
        line2="Plays Wallpaper Engine Workshop scenes. Open source, written in Rust.",
        chips=["Linux · Wayland / X11", "Windows · preview", "MIT / Apache-2.0"],
    ),
    "fr": dict(
        tagline=["Des fonds d’écran animés", "qui dorment quand on ne les voit pas."],
        line2="Lit les scènes du Workshop de Wallpaper Engine. Libre, écrit en Rust.",
        chips=["Linux · Wayland / X11", "Windows · aperçu", "MIT / Apache-2.0"],
    ),
}

FONT = "Lato"


def text(x, y, s, size, fill, weight=400, anchor="start"):
    s = s.replace("&", "&amp;").replace("<", "&lt;")
    return (
        f'<text x="{n(x)}" y="{n(y)}" font-family="{FONT}" font-size="{n(size)}" font-weight="{weight}" '
        f'fill="{fill}" text-anchor="{anchor}">{s}</text>'
    )


def text_width(s, size, weight):
    """Advance width of `s` in Lato, measured with Pillow when it is installed."""
    face = {400: "Regular", 500: "Medium", 600: "Semibold"}[weight]
    try:
        from PIL import ImageFont
    except ImportError:
        return 0.5 * size * len(s)  # Lato's average advance is about half an em
    for path in (f"Lato-{face}.ttf", f"/usr/share/fonts/truetype/lato/Lato-{face}.ttf"):
        try:
            return ImageFont.truetype(path, size).getlength(s)
        except OSError:
            continue
    return 0.5 * size * len(s)


def chip(x, y, label, size=19):
    w = text_width(label, size, 500) + 30
    return (
        f'<rect x="{n(x)}" y="{n(y)}" width="{n(w)}" height="{n(size + 18)}" rx="{n((size + 18) / 2)}" '
        f'fill="#ffffff" fill-opacity="0.06" stroke="#ffffff" stroke-opacity="0.14"/>'
        + text(x + 15, y + size + 3, label, size, MIST, 500)
    ), w


def banner_svg(lang):
    t = BANNER_TEXT[lang]
    W, H = 1280, 640
    hz = 496  # horizon
    scx, sr = 1032, 150
    out = [
        "<defs>",
        f'<linearGradient id="sky" x1="0" y1="0" x2="0" y2="{hz}" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0" stop-color="#14151d"/><stop offset="0.6" stop-color="#1d1b26"/>'
        f'<stop offset="1" stop-color="#3a2519"/></linearGradient>',
        f'<linearGradient id="sea" x1="0" y1="{hz}" x2="0" y2="{H}" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0" stop-color="#16141a"/><stop offset="1" stop-color="{NIGHT}"/></linearGradient>',
        f'<radialGradient id="glow" cx="{scx}" cy="{hz}" r="560" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0" stop-color="{AMBER}" stop-opacity="0.42"/><stop offset="0.4" stop-color="{EMBER}" '
        f'stop-opacity="0.13"/><stop offset="1" stop-color="{EMBER}" stop-opacity="0"/></radialGradient>',
        sun_gradient("sun", hz - 16 - sr * 1.3, hz - 16),
        f'<linearGradient id="hz" x1="0" y1="0" x2="{W}" y2="0" gradientUnits="userSpaceOnUse">'
        f'<stop offset="0" stop-color="{AMBER}" stop-opacity="0"/><stop offset="0.45" stop-color="{AMBER}" '
        f'stop-opacity="0.55"/><stop offset="0.77" stop-color="{AMBER_HI}"/><stop offset="1" '
        f'stop-color="{AMBER}" stop-opacity="0.35"/></linearGradient>',
        "</defs>",
        f'<rect width="{W}" height="{hz}" fill="url(#sky)"/>',
        f'<rect y="{hz}" width="{W}" height="{H - hz}" fill="url(#sea)"/>',
        f'<rect width="{W}" height="{H}" fill="url(#glow)"/>',
        # Same proportion as the symbol: the chord sits 0.3 r below the centre.
        f'<path fill="url(#sun)" d="{sun(scx, hz - 16 - sr * 0.3, sr, hz - 16)}"/>',
        f'<rect y="{hz - 3}" width="{W}" height="6" fill="url(#hz)"/>',
    ]
    for x0, x1, y, w, op in [(-110, 110, 530, 12, 0.8), (-68, 68, 562, 10, 0.5), (-32, 32, 590, 8, 0.3)]:
        out.append(f'<path fill="{AMBER}" fill-opacity="{op}" d="{seg(scx + x0, y, scx + x1, y, w)}"/>')
    # Lockup, tagline, chips (left column, 96 px margin, nothing over the sun).
    out.append(placed_lockup(96, 92, 3.3, MIST, AMBER, EMBER, "lk"))
    out.append(text(96, 262, t["tagline"][0], 44, MIST, 600))
    out.append(text(96, 316, t["tagline"][1], 44, MIST, 600))
    out.append(text(96, 368, t["line2"], 22, MUTED, 400))
    x = 96
    for label in t["chips"]:
        c, w = chip(x, 400, label)
        out.append(c)
        x += w + 12
    return svg_doc(W, H, "".join(out), NAME)


# ------------------------------------------------------------------ brand sheet


def sheet_svg():
    W, H = 1280, 900
    out = [f'<rect width="{W}" height="{H}" fill="{NIGHT}"/>', f'<rect x="640" width="640" height="300" fill="#f6f6f9"/>']
    # Lockups on dark and light.
    for variant, ox, word, line, bottom in (("dark", 0, MIST, AMBER, EMBER), ("light", 640, INK, EMBER_DEEP, EMBER_DEEP)):
        out.append(placed_lockup(ox + 80, 96, 3.05, word, line, bottom, f"s{variant}"))
        out.append(text(ox + 80, 250, "on dark" if variant == "dark" else "on light", 18, MUTED if variant == "dark" else "#4d5060"))
    # Palette.
    swatches = [
        ("Amber", AMBER, "accent (dark)"), ("Ember", EMBER, "sun gradient"), ("Dawn", DAWN, "sun highlight"),
        ("Night", NIGHT, "background (dark)"), ("Mist", MIST, "text (dark)"), ("Ink", INK, "text (light)"),
        ("Dusk", DUSK_TOP, "icon sky"), ("Burnt", "#9a4d00", "accent text (light)"),
    ]
    out.append(text(80, 360, "Palette", 22, MIST, 600))
    for i, (name, hexv, role) in enumerate(swatches):
        x = 80 + i * 142
        out.append(f'<rect x="{x}" y="380" width="126" height="90" rx="14" fill="{hexv}" stroke="#ffffff" stroke-opacity="0.12"/>')
        out.append(text(x, 496, name, 18, MIST, 600))
        out.append(text(x, 518, hexv, 15, MUTED))
        out.append(text(x, 538, role, 13, MUTED))
    # Icon sizes and tray.
    out.append(text(80, 610, "App icon and tray", 22, MIST, 600))
    return W, H, out


# ------------------------------------------------------------------ outputs


def write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def run(cmd):
    subprocess.run(cmd, check=True)


def ico(path, pngs):
    """ICO with PNG-compressed entries (Windows Vista and later read every size)."""
    data = [p.read_bytes() for _, p in pngs]
    head = struct.pack("<HHH", 0, 1, len(pngs))
    offset = 6 + 16 * len(pngs)
    entries = b""
    for (size, _), blob in zip(pngs, data):
        b = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", b, b, 0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
    path.write_bytes(head + entries + b"".join(data))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--svg-only", action="store_true")
    ap.add_argument("--no-packaging", action="store_true", help="do not write packaging/linux/icons")
    ap.add_argument("--resvg", default="resvg")
    ap.add_argument("--usvg", default="usvg")
    args = ap.parse_args()

    logo = ROOT / "logo"
    for v in ("color", "color-light", "mono", "inverse"):
        write(logo / f"{ID}-symbol-{v}.svg", symbol_svg(v))
    for v in ("on-dark", "on-light", "mono", "inverse"):
        write(logo / f"{ID}-logo-{v}.svg", lockup_svg(v))
    for v in ("on-dark", "on-light"):
        write(logo / f"{ID}-logo-stacked-{v}.svg", stacked_svg(v))
    write(logo / f"{ID}-wordmark-mono.svg", wordmark_svg(INK))
    write(logo / f"{ID}-wordmark-inverse.svg", wordmark_svg("#ffffff"))

    icon = ROOT / "icon"
    for size, m in ICON_MASTERS.items():
        name = f"{ID}.svg" if size == 1024 else f"{ID}-{size}.svg"
        write(icon / name, app_icon(size, **m))

    tray = ROOT / "tray"
    write(tray / f"{ID}-symbolic.svg", tray_svg(False))
    write(tray / f"{ID}-paused-symbolic.svg", tray_svg(True))

    if args.svg_only:
        return
    for tool in (args.resvg, args.usvg):
        if not shutil.which(tool):
            sys.exit(f"{tool} not found: cargo install resvg usvg, or pass --resvg/--usvg, or use --svg-only")

    # App icon PNG, ICO, Linux icon theme.
    png = icon / "png"
    png.mkdir(parents=True, exist_ok=True)
    rendered = {}
    for size, master in PNG_SIZES.items():
        src = icon / (f"{ID}.svg" if master == 1024 else f"{ID}-{master}.svg")
        out = png / f"{ID}-{size}.png"
        run([args.resvg, "-w", str(size), "-h", str(size), str(src), str(out)])
        rendered[size] = out
    ico(icon / f"{ID}.ico", [(s, rendered[s]) for s in ICO_SIZES])
    if not args.no_packaging:
        theme = REPO / "packaging" / "linux" / "icons" / "hicolor"
        for size in HICOLOR:
            dst = theme / f"{size}x{size}" / "apps" / f"{ID}.png"
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(rendered[size], dst)
        write(theme / "scalable" / "apps" / f"{ID}.svg", (icon / f"{ID}.svg").read_text())
        write(theme / "symbolic" / "apps" / f"{ID}-symbolic.svg", (tray / f"{ID}-symbolic.svg").read_text())

    # Windows tray: white glyph for dark taskbars, black for light ones.
    tpng = tray / "png"
    tpng.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for paused in (False, True):
            for colour, fill in (("white", "#ffffff"), ("black", "#000000")):
                for size in (16, 20, 24, 32):
                    grid = 24 if size == 24 else 16
                    src = write(tmp / f"t{grid}.svg", tray_flat(paused, fill, grid))
                    name = f"{ID}-tray-{colour}{'-paused' if paused else ''}-{size}.png"
                    run([args.resvg, "-w", str(size), "-h", str(size), str(src), str(tpng / name)])

        # Banners: text converted to outlines by usvg (needs the Lato font installed).
        banner = ROOT / "banner"
        for lang in ("en", "fr"):
            suffix = "" if lang == "en" else "-fr"
            raw = write(tmp / f"banner{suffix}.svg", banner_svg(lang))
            svg = banner / f"{ID}-banner{suffix}.svg"
            svg.parent.mkdir(parents=True, exist_ok=True)
            run([args.usvg, "--font-family", FONT, str(raw), str(svg)])
            run([args.resvg, str(svg), str(banner / f"{ID}-banner{suffix}.png")])

        # Brand sheet (overview image for docs/BRAND.md).
        W, H, out = sheet_svg()
        x = 80
        icons = []
        for size in (128, 64, 48, 32, 24, 16):
            master = PNG_SIZES[size]
            src = icon / (f"{ID}.svg" if master == 1024 else f"{ID}-{master}.svg")
            icons.append(f'<image x="{x}" y="{760 - size}" width="{size}" height="{size}" href="{src.as_posix()}"/>')
            x += size + 28
        x += 40
        for paused in (False, True):
            for fill, bg in ((MIST, None), (INK, "#f6f6f9")):
                if bg:
                    icons.append(f'<rect x="{x - 8}" y="{720 - 8}" width="56" height="56" rx="10" fill="{bg}"/>')
                src = write(tmp / f"sheet-tray-{paused}-{fill[1:]}.svg", tray_flat(paused, fill, 16))
                icons.append(f'<image x="{x}" y="720" width="40" height="40" href="{src.as_posix()}"/>')
                x += 72
        out += icons
        out.append(text(80, 820, "Symbol grid 24 · stroke 2 · round caps · gaps 1.5", 15, MUTED))
        raw = write(tmp / "sheet.svg", svg_doc(W, H, "".join(out), f"{NAME} brand sheet"))
        run([args.usvg, "--font-family", FONT, str(raw), str(tmp / "sheet-flat.svg")])
        run([args.resvg, str(tmp / "sheet-flat.svg"), str(ROOT / "brand-sheet.png")])


if __name__ == "__main__":
    main()
