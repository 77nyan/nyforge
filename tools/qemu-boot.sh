#!/usr/bin/env bash
# Boot a disk image in QEMU and capture the serial log and a screenshot.
#   tools/qemu-boot.sh <disk.img> <bios|uefi|uefi32> <seconds> <outdir> [extra qemu args...]
# Used by the NyBoot test-suite and for debugging. No KVM needed (software emulation).
set -u
IMG=$1; MODE=$2; SECS=$3; OUT=$4; shift 4
mkdir -p "$OUT"
SOCK="$OUT/mon.sock"; rm -f "$SOCK" "$OUT/serial.log" "$OUT/screen.ppm"
COPY="$OUT/disk.img"; cp --sparse=always "$IMG" "$COPY"
ARGS=(-machine accel=tcg -m 768 -display none -no-reboot -serial "file:$OUT/serial.log" -monitor "unix:$SOCK,server,nowait" -drive "file=$COPY,format=raw")
case "$MODE" in
  uefi)
    cp /usr/share/OVMF/OVMF_VARS_4M.fd "$OUT/vars.fd"
    ARGS+=(-drive "if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd" -drive "if=pflash,format=raw,file=$OUT/vars.fd") ;;
  bios) ;;
  *) echo "unknown mode $MODE" >&2; exit 2 ;;
esac
qemu-system-x86_64 "${ARGS[@]}" "$@" &
QPID=$!
# wait for the monitor socket
for _ in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.1; done
END=$((SECONDS + SECS))
while [ $SECONDS -lt $END ] && kill -0 $QPID 2>/dev/null; do sleep 1; done
if kill -0 $QPID 2>/dev/null; then
  python3 - "$SOCK" "$OUT/screen.ppm" <<'PY'
import socket, sys, time
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.settimeout(3)
try: s.recv(4096)
except Exception: pass
s.sendall(("screendump %s\n" % sys.argv[2]).encode()); time.sleep(1.5)
s.sendall(b"quit\n"); time.sleep(0.5)
PY
  kill $QPID 2>/dev/null
fi
wait $QPID 2>/dev/null
[ -s "$OUT/screen.ppm" ] && python3 -c "
from PIL import Image
Image.open('$OUT/screen.ppm').save('$OUT/screen.png')" 2>/dev/null
echo "serial bytes: $(stat -c %s "$OUT/serial.log" 2>/dev/null || echo 0)"
