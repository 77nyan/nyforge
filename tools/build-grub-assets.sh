#!/usr/bin/env bash
# Regenerates the GRUB images NyBoot ships (assets/grub/*).
#
# Needs: grub-mkimage, grub-pc-bin, grub-efi-amd64-bin (and grub-efi-ia32-bin for 32-bit UEFI).
# The committed outputs are what `cargo build` embeds, so normal users never run this.
#
# Every image is self-contained: all modules are built in, and the image reads its
# configuration from the SAME disk it booted from, partition 2 (the small FAT
# "NYBOOT-EFI" partition), file /grub/grub.cfg.  Nothing is stick-specific, so one
# set of blobs works for every drive NyForge makes.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=assets/grub
mkdir -p "$OUT"

# Filesystems we may need to READ ISOs from, plus everything the menu script uses.
COMMON="normal configfile echo test true false regexp search search_label search_fs_uuid search_fs_file \
 part_msdos part_gpt fat exfat ntfs ext2 iso9660 udf loopback ls cat probe blocklist halt reboot sleep \
 loadenv read minicmd gzio xzio lzopio extcmd terminal terminfo serial gfxterm gfxmenu gfxterm_background \
 font png jpeg tga video video_fb all_video gettext datetime date keystatus help tr hexdump \
 memrw cmp test eval source lsmmap boot linux chain"
# NOTE: no ata/ahci/usbms/uhci/...: loading any native disk driver makes GRUB drop the BIOS
# disk services (biosdisk), and then the boot disk "hd0" no longer exists.
BIOS_ONLY="biosdisk linux16 multiboot multiboot2 \
 vbe vga vga_text video_bochs video_cirrus drivemap ntldr bsd"
EFI_ONLY="efi_gop efi_uga efifwsetup efinet lsefi lsefimmap lsefisystab bli fwconfig shim_lock tpm \
 multiboot multiboot2 videoinfo"

# Keep only modules that exist for the target; say which were dropped.
avail() { # <platform> <modules...>
  local plat=$1; shift; local m keep=""
  for m in "$@"; do
    if [ -f "/usr/lib/grub/$plat/$m.mod" ]; then keep="$keep $m"; else echo "  (skipping missing module $m for $plat)" >&2; fi
  done
  echo "$keep"
}

# BIOS: prefix = partition 2 of the boot disk.
grub-mkimage -O i386-pc -o "$OUT/core.img" -p '(,msdos2)/grub' $(avail i386-pc $COMMON $BIOS_ONLY)
cp /usr/lib/grub/i386-pc/boot.img "$OUT/boot.img"

# BIOS boot for Windows install media (single FAT32 partition): prefix = partition 1,
# /boot/grub/grub.cfg runs `ntldr /bootmgr`. Small on purpose.
grub-mkimage -O i386-pc -o "$OUT/core-win.img" -p '(,msdos1)/boot/grub' \
  $(avail i386-pc biosdisk part_msdos fat normal configfile echo test sleep boot ntldr chain terminal minicmd ls cat)

# UEFI: GRUB finds its prefix on the volume it was loaded from.
grub-mkimage -O x86_64-efi -o "$OUT/BOOTX64.EFI" -p /grub $(avail x86_64-efi $COMMON $EFI_ONLY)
if [ -d /usr/lib/grub/i386-efi ]; then
  grub-mkimage -O i386-efi -o "$OUT/BOOTIA32.EFI" -p /grub $(avail i386-efi $COMMON $EFI_ONLY)
fi

cp /usr/share/grub/unicode.pf2 "$OUT/unicode.pf2"
ls -l "$OUT"
