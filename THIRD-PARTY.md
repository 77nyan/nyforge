# Third-party material shipped with NyForge

Rust dependencies are listed in `Cargo.lock`; each is under its own (permissive or GPL-compatible) licence.

- **fatfs** 0.3.6 (`vendor/fatfs`), MIT, with one local fix for long-name entries on "." and ".." directory entries. Licence file kept in that directory.
- **GRUB 2** boot images in `assets/grub` (`BOOTX64.EFI`, `BOOTIA32.EFI`, `boot.img`, `core.img`, `core-win.img`, `unicode.pf2`), GPL-3.0-or-later,
  built from the distribution's GRUB 2 packages by `tools/build-grub-assets.sh`. The corresponding source is the GRUB source of that distribution release
  (https://www.gnu.org/software/grub/); the script documents exactly how each image is produced.
- **egui / eframe** 0.32 (MIT or Apache-2.0) draws the desktop app's window; **rfd** (MIT) opens the system file dialog;
  **windows-sys** (MIT or Apache-2.0) is used for the Windows "restart as administrator" step.
- **Inter** typeface (`assets/fonts`, SIL Open Font License 1.1, copy in `assets/fonts/Inter-LICENSE.txt`) is embedded in the app.
- The NyForge logo (`assets/logo`) is original artwork made for this project by `tools/make-logo.py`, released under the same licence as the code.
