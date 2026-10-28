# Third-party material shipped with NyForge

Rust dependencies are listed in `Cargo.lock`; each is under its own (permissive or GPL-compatible) licence.

- **fatfs** 0.3.6 (`vendor/fatfs`), MIT, with one local fix for long-name entries on "." and ".." directory entries. Licence file kept in that directory.
- **GRUB 2** boot images in `assets/grub` (`BOOTX64.EFI`, `BOOTIA32.EFI`, `boot.img`, `core.img`, `core-win.img`, `unicode.pf2`), GPL-3.0-or-later,
  built from the distribution's GRUB 2 packages by `tools/build-grub-assets.sh`. The corresponding source is the GRUB source of that distribution release
  (https://www.gnu.org/software/grub/); the script documents exactly how each image is produced.
- **UEFI:NTFS** image `assets/uefi-ntfs/uefi-ntfs.img` (1 MiB FAT image: Secure Boot signed UEFI:NTFS boot loaders by Pete Batard, GPL-2.0-or-later; signed NTFS drivers derived
  from ntfs-3g, GPL-2.0-or-later; unsigned exFAT/ARM NTFS drivers from EfiFs, GPL-3.0). Copied unchanged from the Rufus project's repository (`res/uefi/uefi-ntfs.img`,
  SHA-256 `72683fa1250eeea772d3399277b434d4e55ba8dd0dc926e52d817e701fc2eb9e`); its own notes are in `assets/uefi-ntfs/README-from-rufus.txt`. Sources:
  https://github.com/pbatard/uefi-ntfs, https://github.com/pbatard/ntfs-3g, https://github.com/pbatard/efifs. Used to make Windows installers whose files FAT32 cannot hold.
- **egui / eframe** 0.32 (MIT or Apache-2.0) draws the desktop app's window; **rfd** (MIT) opens the system file dialog;
  **windows-sys** (MIT or Apache-2.0) is used for the Windows "restart as administrator" step.
- **Inter** typeface (`assets/fonts`, SIL Open Font License 1.1, copy in `assets/fonts/Inter-LICENSE.txt`) is embedded in the app.
- **Liberation Sans** (`assets/fonts`, SIL Open Font License 1.1, copy in `assets/fonts/LiberationSans-LICENSE.txt`) is converted to GRUB bitmap fonts for the NyBoot boot menu (`assets/grub/theme`).
- **Memtest86+ v7.00** (https://www.memtest.org, source https://github.com/memtest86plus/memtest86plus, GNU GPL v2) is shipped unmodified in `assets/grub/memtest` and copied to the NyBoot boot partition, where the menu starts it as a separate program. Licence: `assets/grub/memtest/LICENSE-GPL-2.txt`.
- The NyForge logo (`assets/logo`) is original artwork made for this project by `tools/make-logo.py`, released under the same licence as the code.
