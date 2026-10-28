#!/usr/bin/env python3
"""Draws the NyForge logo and writes assets/logo/*: nyforge.svg, PNGs, nyforge.ico, nyforge.icns.

The mark is a USB stick on a blue tile, tilted towards a spark (the "forge" flash), with soft lighting, a steel plug, a glossy
body and a glowing spark. The app draws the 64/128/256 px PNGs this script writes (crates/nyforge-gui/src/logo.rs). Needs Pillow and numpy.
"""
import math, os
import numpy as np
from PIL import Image, ImageDraw, ImageFilter

OUT = os.path.join(os.path.dirname(__file__), "..", "assets", "logo")
os.makedirs(OUT, exist_ok=True)

# ---- geometry on a 256 x 256 grid (keep in sync with logo.rs)
TILE = (8, 8, 248, 248); TILE_R = 56
PIVOT = (126, 136)          # the stick is drawn around this point, then turned by ANGLE degrees clockwise
ANGLE = 38.0
BODY = (-31, -30, 31, 66, 13)      # x0, y0, x1, y1, corner radius
PLUG = (-22, -80, 22, -24, 4)
PINS = [(-13, -70, -5, -48, 1.5), (5, -70, 13, -48, 1.5)]
HOLE = (0, 47, 6.5)                # cx, cy, r
SPARK = (207, 49, 14)              # centre x, y and radius of a four-point star (grid coordinates)
SHADOW = (7, 11)                   # offset of the soft shadow under the stick
C_TOP = (0x5C, 0xC0, 0xFF)
C_BOT = (0x1C, 0x74, 0xD4)
C_BODY = (0xFF, 0xFF, 0xFF)
C_PLUG = (0xD3, 0xDD, 0xEA)
C_PIN = (0x2F, 0x3B, 0x4E)
C_HOLE = (0x2A, 0x7F, 0xDA)
C_SPARK = (0xFF, 0xC8, 0x3D)

def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))

def rrect(x0, y0, x1, y1, r, seg=10):
    pts = []
    for cx, cy, a0 in ((x1 - r, y0 + r, -90), (x1 - r, y1 - r, 0), (x0 + r, y1 - r, 90), (x0 + r, y0 + r, 180)):
        for i in range(seg + 1):
            a = math.radians(a0 + 90 * i / seg)
            pts.append((cx + r * math.cos(a), cy + r * math.sin(a)))
    return pts

def turn(pts):
    a = math.radians(ANGLE)
    ca, sa = math.cos(a), math.sin(a)
    return [(PIVOT[0] + x * ca - y * sa, PIVOT[1] + x * sa + y * ca) for x, y in pts]

def star(cx, cy, r):
    k = r * 0.28
    return [(cx, cy - r), (cx + k, cy - k), (cx + r, cy), (cx + k, cy + k), (cx, cy + r), (cx - k, cy + k), (cx - r, cy), (cx - k, cy - k)]

def circle(cx, cy, r, seg=24):
    return [(cx + r * math.cos(2 * math.pi * i / seg), cy + r * math.sin(2 * math.pi * i / seg)) for i in range(seg)]

def _mask(W, polys, blur=0.0):
    m = Image.new("L", (W, W), 0)
    d = ImageDraw.Draw(m)
    for p in polys:
        d.polygon(p, fill=255)
    return m.filter(ImageFilter.GaussianBlur(blur)) if blur else m

def _grad(W, fn):
    """RGBA image whose pixels come from fn(x, y) -> arrays (r, g, b, a) on normalised 0..1 grid coordinates."""
    ys, xs = np.mgrid[0:W, 0:W].astype("float32")
    r, g, b, a = fn(xs / W, ys / W)
    out = np.stack([np.broadcast_to(c, (W, W)) for c in (r, g, b, a)], axis=-1)
    return Image.fromarray(np.clip(out, 0, 255).astype("uint8"), "RGBA")

def _mix(c0, c1, t):
    return [c0[i] + (c1[i] - c0[i]) * t for i in range(3)]

def _paste(img, layer, mask):
    """Composite `layer` through `mask` onto img."""
    l = layer.copy()
    l.putalpha(Image.fromarray((np.asarray(layer.getchannel("A"), dtype="float32") * np.asarray(mask, dtype="float32") / 255).astype("uint8")))
    return Image.alpha_composite(img, l)

def render(px):
    ss = 4 if px <= 512 else 2
    W = px * ss
    f = W / 256.0
    sc = lambda pts: [(x * f, y * f) for x, y in pts]
    big = px >= 48
    img = Image.new("RGBA", (W, W), (0, 0, 0, 0))

    # ---- the tile: diagonal gradient, soft light from the top, glossy sheen, rim light
    tile_poly = sc(rrect(TILE[0], TILE[1], TILE[2], TILE[3], TILE_R, 14))
    tile_mask = _mask(W, [tile_poly])
    t_hi, t_lo = (0x7A, 0xD2, 0xFF), (0x16, 0x62, 0xC8)
    def tile_fn(x, y):
        t = np.clip(0.62 * y + 0.38 * x, 0, 1)
        c = _mix(t_hi, t_lo, t)
        glow = np.clip(1 - np.hypot((x - 0.38) / 0.62, (y - 0.02) / 0.55), 0, 1) ** 1.6 * 0.55   # light from above
        vig = np.clip((y - 0.62) / 0.38, 0, 1) ** 1.5 * 0.30                                       # darker toward the bottom
        c = [np.clip(ch + (255 - ch) * glow, 0, 255) * (1 - vig) + 6 * vig for ch in c]
        return c[0], c[1], c[2], np.full_like(x, 255)
    img = _paste(img, _grad(W, tile_fn), tile_mask)
    if big:
        sheen = _grad(W, lambda x, y: (np.full_like(x, 255), np.full_like(x, 255), np.full_like(x, 255), np.clip(1 - y / 0.42, 0, 1) ** 1.4 * 64 * np.clip(1.15 - np.abs(x - 0.45) * 1.1, 0, 1)))
        sheen_mask = _mask(W, [sc(rrect(TILE[0] + 5, TILE[1] + 4, TILE[2] - 5, 118, TILE_R - 6, 12))])
        img = _paste(img, sheen, sheen_mask)
        inner = _mask(W, [sc(rrect(TILE[0] + 3.2, TILE[1] + 3.2, TILE[2] - 3.2, TILE[3] - 3.2, TILE_R - 3, 14))])
        ring = Image.fromarray((np.clip(np.asarray(tile_mask, dtype="int16") - np.asarray(inner, dtype="int16"), 0, 255)).astype("uint8"))
        rim = _grad(W, lambda x, y: (np.full_like(x, 255), np.full_like(x, 255), np.full_like(x, 255), np.clip(1 - y * 1.25, 0.10, 1) * 150))
        img = _paste(img, rim, ring)

    # ---- the stick, drawn upright around the pivot, then turned
    P = PIVOT
    loc = lambda r: (r[0] + P[0], r[1] + P[1], r[2] + P[0], r[3] + P[1], r[4])
    body_p = sc(rrect(*loc(BODY)))
    plug_p = sc(rrect(*loc(PLUG), seg=6))
    body_m, plug_m = _mask(W, [body_p]), _mask(W, [plug_p])
    X0, X1 = (P[0] + BODY[0]) * f, (P[0] + BODY[2]) * f
    Y0, Y1 = (P[1] + BODY[1]) * f, (P[1] + BODY[3]) * f
    stick = Image.new("RGBA", (W, W), (0, 0, 0, 0))

    # plug: brushed steel
    px0, px1 = (P[0] + PLUG[0]) * f, (P[0] + PLUG[2]) * f
    py0, py1 = (P[1] + PLUG[1]) * f, (P[1] + PLUG[3]) * f
    def plug_fn(x, y):
        u = np.clip((x * W - px0) / (px1 - px0), 0, 1)
        v = np.clip((y * W - py0) / (py1 - py0), 0, 1)
        band = 0.5 + 0.5 * np.cos((u - 0.30) * 2 * np.pi * 0.9)
        base = _mix((0xA4, 0xB3, 0xC7), (0xF4, 0xF8, 0xFD), band)
        shade = 0.20 * v ** 2                                    # darker where it goes into the body
        c = [ch * (1 - shade) for ch in base]
        return c[0], c[1], c[2], np.full_like(x, 255)
    stick = _paste(stick, _grad(W, plug_fn), plug_m)
    # the dark socket the plug sits in (a soft shadow on the plug just above the body)
    cast = _grad(W, lambda x, y: (np.full_like(x, 20), np.full_like(x, 40), np.full_like(x, 80), np.clip(1 - np.abs(y * W - (Y0 - 1 * f)) / (9 * f), 0, 1) * 120))
    stick = _paste(stick, cast, plug_m)
    # the two contacts
    for p in PINS:
        pp = sc(rrect(*loc(p), seg=4))
        pm = _mask(W, [pp])
        pgrad = _grad(W, lambda x, y, p=p: (*[ch * np.ones_like(x) for ch in _mix((0x4A, 0x58, 0x70), (0x1C, 0x26, 0x38), np.clip((y * W - (P[1] + p[1]) * f) / ((p[3] - p[1]) * f), 0, 1))], np.full_like(x, 255)))
        stick = _paste(stick, pgrad, pm)
        if big:
            hl = _grad(W, lambda x, y: (np.full_like(x, 255), np.full_like(x, 255), np.full_like(x, 255), np.full_like(x, 70)))
            hm = _mask(W, [sc(rrect(P[0] + p[0], P[1] + p[1], P[0] + p[0] + 1.6, P[1] + p[3], 0.8, 3))])
            stick = _paste(stick, hl, hm)

    # body: soft white plastic with a cool shade on the right and a bright edge on the left
    def body_fn(x, y):
        u = np.clip((x * W - X0) / (X1 - X0), 0, 1)
        v = np.clip((y * W - Y0) / (Y1 - Y0), 0, 1)
        base = _mix((0xFF, 0xFF, 0xFF), (0xC9, 0xD9, 0xEC), np.clip(u ** 1.6 * 0.95 + v * 0.25, 0, 1))
        return base[0], base[1], base[2], np.full_like(x, 255)
    stick = _paste(stick, _grad(W, body_fn), body_m)
    if big:
        # contact shadow where the plug enters
        top = _grad(W, lambda x, y: (np.full_like(x, 30), np.full_like(x, 60), np.full_like(x, 110), np.clip(1 - (y * W - Y0) / (12 * f), 0, 1) ** 1.5 * 70))
        stick = _paste(stick, top, body_m)
        # gloss strip along the left edge
        gl = _grad(W, lambda x, y: (np.full_like(x, 255), np.full_like(x, 255), np.full_like(x, 255), np.clip(1 - np.abs(x * W - (X0 + 9 * f)) / (6 * f), 0, 1) * 150 * np.clip(1 - (y * W - Y0) / (Y1 - Y0) * 0.55, 0, 1)))
        stick = _paste(stick, gl, body_m)
        # soft rim along the bottom
        rimb = _grad(W, lambda x, y: (np.full_like(x, 90), np.full_like(x, 130), np.full_like(x, 190), np.clip((y * W - (Y1 - 14 * f)) / (14 * f), 0, 1) ** 2 * 80))
        stick = _paste(stick, rimb, body_m)
    # the lanyard hole: a sunken ring
    hx, hy = P[0] + HOLE[0], P[1] + HOLE[1]
    ring_o = _mask(W, [sc(circle(hx, hy, HOLE[2] + 1.6, 40))])
    stick = _paste(stick, _grad(W, lambda x, y: (np.full_like(x, 150), np.full_like(x, 180), np.full_like(x, 215), np.full_like(x, 255))), ring_o)
    hole_m = _mask(W, [sc(circle(hx, hy, HOLE[2], 40))])
    hole = _grad(W, lambda x, y: (*_mix((0x1E, 0x6A, 0xC4), (0x4A, 0xA0, 0xF0), np.clip((y * W - (hy - HOLE[2]) * f) / (2 * HOLE[2] * f), 0, 1)), np.full_like(x, 255)))
    stick = _paste(stick, hole, hole_m)

    # turn the stick
    stick_t = stick.rotate(-ANGLE, resample=Image.BICUBIC, center=(P[0] * f, P[1] * f))
    alpha_t = stick_t.getchannel("A")

    # shadows under the stick: wide and soft, plus a tight one close to the edge
    shape_t = Image.new("L", (W, W), 0)
    shape_t.paste(alpha_t)
    for (dx, dy, blur, al) in ((SHADOW[0], SHADOW[1], 6.0, 105), (SHADOW[0] * 0.45, SHADOW[1] * 0.45, 2.4, 80)):
        sh = Image.new("RGBA", (W, W), (6, 34, 96, 0))
        sh.putalpha(shape_t.point(lambda v, al=al: v * al // 255))
        sh = ImageChops_offset(sh, dx * f, dy * f).filter(ImageFilter.GaussianBlur(blur * f))
        img = _paste(img, sh, tile_mask)
    img = Image.alpha_composite(img, stick_t)

    # warm light from the spark falling on the stick
    sx, sy, sr = SPARK
    if big:
        warm = _grad(W, lambda x, y: (np.full_like(x, 255), np.full_like(x, 214), np.full_like(x, 120), np.clip(1 - np.hypot(x * W - sx * f, y * W - sy * f) / (70 * f), 0, 1) ** 2 * 120))
        img = _paste(img, warm, alpha_t)

    # the spark: glow, then a gold star with a white heart
    glow = Image.new("RGBA", (W, W), (255, 200, 60, 0))
    gm = _mask(W, [sc(circle(sx, sy, sr * 1.15, 40))], blur=sr * 0.9 * f)
    glow.putalpha(gm.point(lambda v: v * 190 // 255))
    img = _paste(img, glow, tile_mask)
    star_poly = sc(star(sx, sy, sr))
    star_m = _mask(W, [star_poly])
    star_c = _grad(W, lambda x, y: (*_mix((0xFF, 0xE0, 0x6A), (0xFF, 0xB0, 0x20), np.clip(np.hypot(x * W - sx * f, y * W - sy * f) / (sr * f), 0, 1)), np.full_like(x, 255)))
    img = _paste(img, star_c, star_m)
    core = _mask(W, [sc(star(sx, sy, sr * 0.52))], blur=0.6 * f)
    img = _paste(img, _grad(W, lambda x, y: (np.full_like(x, 255), np.full_like(x, 252), np.full_like(x, 235), np.full_like(x, 235))), core)
    return img.resize((px, px), Image.LANCZOS)

def ImageChops_offset(im, dx, dy):
    out = Image.new(im.mode, im.size, (0, 0, 0, 0))
    out.paste(im, (round(dx), round(dy)))
    return out

def svg():
    """The same picture as vectors: gradients, soft shadows, a glow. (The PNGs are drawn by render(); this matches it closely.)"""
    h = lambda c: "#%02X%02X%02X" % c
    rect = lambda r, fill, extra="": f'<rect x="{r[0]}" y="{r[1]}" width="{r[2]-r[0]}" height="{r[3]-r[1]}" rx="{r[4]}" fill="{fill}"{extra}/>'
    st = " ".join(f"{x},{y}" for x, y in star(*SPARK))
    core = " ".join(f"{x},{y}" for x, y in star(SPARK[0], SPARK[1], SPARK[2] * 0.52))
    t = f'translate({PIVOT[0]} {PIVOT[1]}) rotate({ANGLE})'
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" width="256" height="256" role="img" aria-label="NyForge">
  <defs>
    <linearGradient id="tile" x1="0.2" y1="0" x2="0.8" y2="1"><stop offset="0" stop-color="#7AD2FF"/><stop offset="1" stop-color="#1662C8"/></linearGradient>
    <radialGradient id="light" cx="0.38" cy="0.02" r="0.7"><stop offset="0" stop-color="#FFFFFF" stop-opacity="0.55"/><stop offset="1" stop-color="#FFFFFF" stop-opacity="0"/></radialGradient>
    <linearGradient id="sheen" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#FFFFFF" stop-opacity="0.26"/><stop offset="1" stop-color="#FFFFFF" stop-opacity="0"/></linearGradient>
    <linearGradient id="rim" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#FFFFFF" stop-opacity="0.6"/><stop offset="0.8" stop-color="#FFFFFF" stop-opacity="0.08"/></linearGradient>
    <linearGradient id="body" x1="0" y1="0" x2="1" y2="0.35"><stop offset="0" stop-color="#FFFFFF"/><stop offset="0.45" stop-color="#F4F8FD"/><stop offset="1" stop-color="#C9D9EC"/></linearGradient>
    <linearGradient id="steel" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#A4B3C7"/><stop offset="0.3" stop-color="#F4F8FD"/><stop offset="0.75" stop-color="#B9C6D8"/><stop offset="1" stop-color="#E6EDF6"/></linearGradient>
    <linearGradient id="pin" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#4A5870"/><stop offset="1" stop-color="#1C2638"/></linearGradient>
    <linearGradient id="gloss" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#FFFFFF" stop-opacity="0"/><stop offset="0.5" stop-color="#FFFFFF" stop-opacity="0.6"/><stop offset="1" stop-color="#FFFFFF" stop-opacity="0"/></linearGradient>
    <radialGradient id="star" cx="0.5" cy="0.5" r="0.5"><stop offset="0" stop-color="#FFE06A"/><stop offset="1" stop-color="#FFB020"/></radialGradient>
    <radialGradient id="glow" cx="0.5" cy="0.5" r="0.5"><stop offset="0" stop-color="#FFC83D" stop-opacity="0.75"/><stop offset="1" stop-color="#FFC83D" stop-opacity="0"/></radialGradient>
    <filter id="soft" x="-30%" y="-30%" width="160%" height="160%"><feGaussianBlur stdDeviation="6"/></filter>
    <filter id="tight" x="-30%" y="-30%" width="160%" height="160%"><feGaussianBlur stdDeviation="2.4"/></filter>
    <clipPath id="clip"><rect x="{TILE[0]}" y="{TILE[1]}" width="{TILE[2]-TILE[0]}" height="{TILE[3]-TILE[1]}" rx="{TILE_R}"/></clipPath>
  </defs>
  <rect x="{TILE[0]}" y="{TILE[1]}" width="{TILE[2]-TILE[0]}" height="{TILE[3]-TILE[1]}" rx="{TILE_R}" fill="url(#tile)"/>
  <g clip-path="url(#clip)">
    <rect x="0" y="0" width="256" height="256" fill="url(#light)"/>
    <rect x="13" y="12" width="230" height="106" rx="50" fill="url(#sheen)"/>
    <g transform="translate({SHADOW[0]} {SHADOW[1]}) {t}" filter="url(#soft)" fill="#062260" fill-opacity="0.42">
      {rect(BODY, "#062260")}{rect(PLUG, "#062260")}
    </g>
    <g transform="translate({SHADOW[0]*0.45} {SHADOW[1]*0.45}) {t}" filter="url(#tight)" fill="#062260" fill-opacity="0.32">
      {rect(BODY, "#062260")}{rect(PLUG, "#062260")}
    </g>
  </g>
  <rect x="{TILE[0]+1.6}" y="{TILE[1]+1.6}" width="{TILE[2]-TILE[0]-3.2}" height="{TILE[3]-TILE[1]-3.2}" rx="{TILE_R-1.6}" fill="none" stroke="url(#rim)" stroke-width="3.2"/>
  <g transform="{t}">
    {rect(PLUG, "url(#steel)")}
    {rect(BODY, "url(#body)")}
    <rect x="{BODY[0]+6}" y="{BODY[1]+8}" width="7" height="{BODY[3]-BODY[1]-30}" rx="3.5" fill="url(#gloss)"/>
    {rect(PINS[0], "url(#pin)")}
    {rect(PINS[1], "url(#pin)")}
    <circle cx="{HOLE[0]}" cy="{HOLE[1]}" r="{HOLE[2]+1.6}" fill="#96B4D7"/>
    <circle cx="{HOLE[0]}" cy="{HOLE[1]}" r="{HOLE[2]}" fill="#2A7FDA"/>
  </g>
  <circle cx="{SPARK[0]}" cy="{SPARK[1]}" r="{SPARK[2]*2.2}" fill="url(#glow)"/>
  <polygon points="{st}" fill="url(#star)"/>
  <polygon points="{core}" fill="#FFFCEB" fill-opacity="0.92"/>
</svg>
'''

if __name__ == "__main__":
    open(os.path.join(OUT, "nyforge.svg"), "w").write(svg())
    sizes = [16, 24, 32, 48, 64, 128, 256, 512, 1024]
    imgs = {n: render(n) for n in sizes}
    for n in (32, 64, 128, 256, 512):
        imgs[n].save(os.path.join(OUT, f"nyforge-{n}.png"))
    imgs[256].save(os.path.join(OUT, "nyforge.ico"), sizes=[(n, n) for n in (16, 24, 32, 48, 64, 128, 256)])
    imgs[1024].save(os.path.join(OUT, "nyforge.icns"))
    print("wrote", sorted(os.listdir(OUT)))
