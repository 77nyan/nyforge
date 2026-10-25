#!/usr/bin/env python3
"""Draws the NyForge logo and writes assets/logo/*: nyforge.svg, PNGs, nyforge.ico, nyforge.icns.

The mark is a USB stick on a blue tile, tilted towards a spark (the "forge" flash). The same numbers are used
by crates/nyforge-gui/src/logo.rs so the window draws the identical shape as vectors. Needs Pillow.
"""
import math, os
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

def render(px):
    ss = 4
    W = px * ss
    f = px / 256.0 * ss
    sc = lambda pts: [(x * f, y * f) for x, y in pts]
    img = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    # tile with a vertical gradient
    grad = Image.new("RGBA", (W, W))
    gd = ImageDraw.Draw(grad)
    for y in range(W):
        gd.line([(0, y), (W, y)], fill=lerp(C_TOP, C_BOT, y / W) + (255,))
    mask = Image.new("L", (W, W), 0)
    ImageDraw.Draw(mask).rounded_rectangle([v * f for v in TILE], radius=TILE_R * f, fill=255)
    img.paste(grad, (0, 0), mask)
    if px >= 48:
        ImageDraw.Draw(img).rounded_rectangle([v * f for v in TILE], radius=TILE_R * f, outline=(255, 255, 255, 46), width=max(1, round(2.5 * f)))
    # soft shadow of the stick
    sh = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    sd = ImageDraw.Draw(sh)
    for shape in (BODY, PLUG):
        pts = [(x + SHADOW[0], y + SHADOW[1]) for x, y in turn(rrect(*shape))]
        sd.polygon(sc(pts), fill=(8, 40, 100, 90))
    sh = sh.filter(ImageFilter.GaussianBlur(5 * f))
    clip = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    clip.paste(sh, (0, 0), mask)
    img = Image.alpha_composite(img, clip)
    d = ImageDraw.Draw(img)
    d.polygon(sc(turn(rrect(*PLUG))), fill=C_PLUG + (255,))
    d.polygon(sc(turn(rrect(*BODY))), fill=C_BODY + (255,))
    for p in PINS:
        d.polygon(sc(turn(rrect(*p, seg=4))), fill=C_PIN + (255,))
    hx, hy = turn([(HOLE[0], HOLE[1])])[0]
    d.polygon(sc(circle(hx, hy, HOLE[2])), fill=C_HOLE + (255,))
    d.polygon(sc(star(*SPARK)), fill=C_SPARK + (255,))
    return img.resize((px, px), Image.LANCZOS)

def svg():
    h = lambda c: "#%02X%02X%02X" % c
    rect = lambda r, fill: f'<rect x="{r[0]}" y="{r[1]}" width="{r[2]-r[0]}" height="{r[3]-r[1]}" rx="{r[4]}" fill="{fill}"/>'
    st = " ".join(f"{x},{y}" for x, y in star(*SPARK))
    t = f'translate({PIVOT[0]} {PIVOT[1]}) rotate({ANGLE})'
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" width="256" height="256" role="img" aria-label="NyForge">
  <defs>
    <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{h(C_TOP)}"/><stop offset="1" stop-color="{h(C_BOT)}"/></linearGradient>
    <filter id="soft" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation="5"/></filter>
  </defs>
  <rect x="{TILE[0]}" y="{TILE[1]}" width="{TILE[2]-TILE[0]}" height="{TILE[3]-TILE[1]}" rx="{TILE_R}" fill="url(#tile)"/>
  <g transform="translate({SHADOW[0]} {SHADOW[1]}) {t}" filter="url(#soft)" fill="#082864" fill-opacity="0.35">
    <rect x="{BODY[0]}" y="{BODY[1]}" width="{BODY[2]-BODY[0]}" height="{BODY[3]-BODY[1]}" rx="{BODY[4]}"/>
    <rect x="{PLUG[0]}" y="{PLUG[1]}" width="{PLUG[2]-PLUG[0]}" height="{PLUG[3]-PLUG[1]}" rx="{PLUG[4]}"/>
  </g>
  <g transform="{t}">
    {rect(PLUG, h(C_PLUG))}
    {rect(BODY, h(C_BODY))}
    {rect(PINS[0], h(C_PIN))}
    {rect(PINS[1], h(C_PIN))}
    <circle cx="{HOLE[0]}" cy="{HOLE[1]}" r="{HOLE[2]}" fill="{h(C_HOLE)}"/>
  </g>
  <polygon points="{st}" fill="{h(C_SPARK)}"/>
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
