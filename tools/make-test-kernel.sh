#!/usr/bin/env bash
# Builds the tiny kernel+initramfs the "real kernel" NyBoot test boots.
#   tools/make-test-kernel.sh <outdir>      then:  NYFORGE_TEST_KERNEL_DIR=<outdir> cargo test ... -- --ignored
# Needs: apt (to download Ubuntu's kernel .deb), dpkg-deb, busybox (static), cpio, gzip.
set -euo pipefail
OUT=${1:?usage: $0 <outdir>}
mkdir -p "$OUT/work" && cd "$OUT/work"
apt-get download linux-image-virtual >/dev/null 2>&1 || true
KVER=$(apt-cache depends linux-image-virtual | awk '/Depends: linux-image-/{print $2; exit}')
apt-get download "$KVER" >/dev/null 2>&1
dpkg-deb -x "$KVER"_*.deb x
cp x/boot/vmlinuz-* "$OUT/vmlinuz"
mkdir -p rd/bin rd/proc rd/sys rd/dev
cp "$(command -v busybox)" rd/bin/busybox
cat > rd/init <<'INIT'
#!/bin/busybox sh
/bin/busybox mount -t proc proc /proc
echo "NBX-REAL-KERNEL-BOOTED"
echo "NBX-CMDLINE: $(/bin/busybox cat /proc/cmdline)"
/bin/busybox poweroff -f
INIT
chmod +x rd/init
(cd rd && find . | cpio -o -H newc 2>/dev/null | gzip -9 > "$OUT/initrd.gz")
echo "Wrote $OUT/vmlinuz and $OUT/initrd.gz"
