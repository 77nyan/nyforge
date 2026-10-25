#!/usr/bin/env python3
"""Boot an image in QEMU, type lines into the guest keyboard, save screenshots.
usage: qemu_type.py disk.img bios|uefi outdir wait_secs "line1" "line2" ...
Each typed line is followed by Enter and a screenshot out-N.png is taken after it."""
import os, socket, subprocess, sys, time, shutil
img, mode, out, wait = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
lines = sys.argv[5:]
os.makedirs(out, exist_ok=True)
sock = os.path.join(out, "mon.sock")
if os.path.exists(sock): os.remove(sock)
disk = os.path.join(out, "disk.img")
subprocess.run(["cp", "--sparse=always", img, disk], check=True)
args = ["qemu-system-x86_64", "-machine", "accel=tcg", "-m", "768", "-display", "none", "-no-reboot",
        "-serial", "file:" + os.path.join(out, "serial.log"), "-monitor", f"unix:{sock},server,nowait",
        "-drive", f"file={disk},format=raw"]
if mode == "uefi":
    shutil.copy("/usr/share/OVMF/OVMF_VARS_4M.fd", os.path.join(out, "vars.fd"))
    args += ["-drive", "if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd",
             "-drive", f"if=pflash,format=raw,file={os.path.join(out,'vars.fd')}"]
p = subprocess.Popen(args)
for _ in range(100):
    if os.path.exists(sock): break
    time.sleep(0.1)
s = socket.socket(socket.AF_UNIX); s.connect(sock); s.settimeout(5)
try: s.recv(4096)
except Exception: pass
def mon(cmd, delay=0.15):
    s.sendall((cmd + "\n").encode()); time.sleep(delay)
    try: s.recv(65536)
    except Exception: pass
KEYS = {" ": "spc", "/": "slash", ".": "dot", "-": "minus", "=": "equal", ",": "comma", "(": "shift-9", ")": "shift-0",
        "$": "shift-4", "_": "shift-minus", ":": "shift-semicolon", ";": "semicolon", "\"": "shift-apostrophe",
        "'": "apostrophe", "*": "shift-8", "|": "shift-backslash", "\\": "backslash", "<": "shift-comma",
        ">": "shift-dot", "?": "shift-slash", "!": "shift-1", "@": "shift-2", "#": "shift-3", "%": "shift-5",
        "&": "shift-7", "+": "shift-equal", "[": "bracket_left", "]": "bracket_right", "{": "shift-bracket_left",
        "}": "shift-bracket_right", "~": "shift-grave_accent", "^": "shift-6", "`": "grave_accent"}
def key(ch):
    if ch in KEYS: return KEYS[ch]
    if ch.isupper(): return "shift-" + ch.lower()
    return ch
time.sleep(wait)
def shot(name):
    ppm = os.path.join(out, name + ".ppm"); mon(f"screendump {ppm}", 1.2)
    try:
        from PIL import Image
        Image.open(ppm).save(os.path.join(out, name + ".png"))
    except Exception as e: print("shot failed", e)
shot("out-0")
for i, line in enumerate(lines, 1):
    for ch in line:
        mon("sendkey " + key(ch), 0.06)
    mon("sendkey ret", 0.5)
    time.sleep(2.0)
    shot(f"out-{i}")
mon("quit")
p.wait(timeout=10)
