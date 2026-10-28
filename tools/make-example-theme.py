#!/usr/bin/env python3
"""Writes docs/themes/light/: a small example of a custom NyBoot theme (a light one, 1080p sizes).
It is an ordinary GRUB 2 theme, so it also shows what a theme folder needs: theme.txt, the pictures it names, the .pf2 fonts.
Needs Pillow and grub-mkfont. Run:  python3 tools/make-example-theme.py
"""
import os
import subprocess
from PIL import Image

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "docs", "themes", "light")
FONT = os.path.join(ROOT, "assets", "fonts", "LiberationSans-Regular.ttf")
BOLD = os.path.join(ROOT, "assets", "fonts", "LiberationSans-Bold.ttf")
os.makedirs(OUT, exist_ok=True)

PAGE, PANEL, EDGE, SELECT, ACCENT = (0xe9, 0xec, 0xf1), (0xf8, 0xf9, 0xfb), (0xc3, 0xc9, 0xd4), (0xd6, 0xe4, 0xf8), (0x1f, 0x6f, 0xd0)


def px(c, w=1, h=1):
    return Image.new("RGB", (w, h), c)


def frame(prefix, fill, edge):
    for k in ("nw", "ne", "sw", "se"):
        px(edge).save(f"{OUT}/{prefix}_{k}.png")
    for k in ("n", "s", "w", "e"):
        px(edge).save(f"{OUT}/{prefix}_{k}.png")
    px(fill).save(f"{OUT}/{prefix}_c.png")


frame("menu", PANEL, EDGE)
frame("terminal_box", (0x16, 0x19, 0x1f), EDGE)
w = Image.new("RGB", (14, 46), SELECT)
w.paste(px(ACCENT, 3, 46), (0, 0))
w.save(f"{OUT}/select_w.png")
px(SELECT, 1, 46).save(f"{OUT}/select_c.png")
px(SELECT, 14, 46).save(f"{OUT}/select_e.png")
for k in ("c", "n", "s"):
    px((0xdd, 0xe1, 0xe8)).save(f"{OUT}/scrollbar_frame_{k}.png")
    px((0x9a, 0xa3, 0xb3)).save(f"{OUT}/scrollbar_thumb_{k}.png")

ranges = "0x20-0x7e,0xa0-0x24f,0x2022-0x2022,0x2190-0x2193"
for style, src, size in (("regular", FONT, 24), ("bold", BOLD, 24), ("regular", FONT, 15), ("regular", FONT, 17)):
    subprocess.check_call(["grub-mkfont", "-s", str(size), "-r", ranges, "-o", f"{OUT}/font-{style}-{size}.pf2", src], stderr=subprocess.DEVNULL)

open(f"{OUT}/theme.txt", "w").write('''# Example NyBoot theme: light. A plain GRUB 2 theme; copy the folder with
#   nyforge boot theme <drive> docs/themes/light
# Every .pf2 font in this folder is loaded for you. Fonts are named "<family> <style> <size>".

title-text: ""
desktop-color: "#e9ecf1"

terminal-font: "GNU Unifont Regular 16"
terminal-left: "10%"
terminal-top: "10%"
terminal-width: "80%"
terminal-height: "80%"
terminal-border: "0"
terminal-box: "terminal_box_*.png"

+ label {
    id = "keys"
    left = 24
    top = 100%-52
    width = 70%
    height = 24
    text = "↑ ↓  Choose     Enter  Start     E  Edit     C  Command line"
    font = "Liberation Sans Regular 15"
    color = "#5b6578"
    align = "left"
}

+ boot_menu {
    left = 15%
    top = 12%
    width = 70%
    height = 76%
    item_font = "Liberation Sans Regular 24"
    selected_item_font = "Liberation Sans Bold 24"
    item_color = "#2a3140"
    selected_item_color = "#0d1b33"
    icon_width = 0
    icon_height = 0
    item_icon_space = 0
    item_height = 46
    item_padding = 12
    item_spacing = 2
    menu_pixmap_style = "menu_*.png"
    selected_item_pixmap_style = "select_*.png"
    scrollbar = true
    scrollbar_width = 6
    scrollbar_thumb = "scrollbar_thumb_*.png"
    scrollbar_frame = "scrollbar_frame_*.png"
}

+ label {
    id = "__timeout__"
    left = 0
    top = 90%
    width = 100%
    height = 24
    text = "Starting automatically in %d seconds"
    font = "Liberation Sans Regular 17"
    color = "#5b6578"
    align = "center"
}
''')
print(sum(os.path.getsize(f"{OUT}/{f}") for f in os.listdir(OUT)) // 1024, "KiB in", OUT)
