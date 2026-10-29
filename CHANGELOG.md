# Changelog

## 0.1.9

### Added
- **Terminal app.** `nyforge -t` (also `--t` and `-terminal`) opens a full-screen app with NyFlash, NyBoot, NyFormat, NyTest and the version history, a menu on the left and the tool on the right. It runs the same `nyforge` engine as the window, so the same safety rules apply: the system drive is never offered, a drive is checked again just before anything is erased, and an erase has to be confirmed. It asks for `sudo` once, only when a job needs it, and uses about 4 MB of memory. It is installed as `nyforge-tui` next to `nyforge`.
- **A new installer screen.** `sudo ./install.sh` now opens a small step-by-step screen in the terminal (Install NyForge? Yes / No, what to install, then each step with a green check), usable with the mouse or the keyboard. `--yes` skips it.

### Changed
- **`sudo ./install.sh` is the only way to install on Linux.** `sh install.sh`, `bash install.sh`, `./install.sh` without sudo and `sudo sh install.sh` stop with `To install the program run: sudo ./install.sh`.

## 0.1.82

### Changed
- **NyBoot and Secure Boot.** NyBoot's boot files are not signed, so a computer with Secure Boot on lists the drive but silently starts its own system instead. The NyBoot screen now says so after an install or update, and the README explains how to check (`mokutil --sb-state`) and what to do (turn Secure Boot off in the firmware settings).

## 0.1.81

### Changed
- **NyBoot menu text is smaller.** The list text was 24 px on a 1080p screen, which looked oversized; it is now 20 px with tighter rows (the 1440p and 4K sizes scale with it).
- **Cleaner letters.** The GRUB menu can only draw text in one colour per pixel (no smoothing), so the fonts are now generated with FreeType's auto-hinter, which snaps strokes to whole pixels. Edges and spacing are much more even.

### Added
- **Click the version at the bottom of Settings** to see what changed in every version, from 0.1.0 on.

## 0.1.8

### Fixed
- **exFAT drives made by NyForge could not be opened** by Linux file managers ("Unable to access ... wrong fs type, bad option, bad superblock"). The root directory started with an empty slot where the volume label belongs, and Linux's exFAT driver stops reading there, so it never found the allocation bitmap. The label entry is now always written, as `mkfs.exfat` does. **Drives formatted with 0.1.7 need to be formatted again.** A new test checks the layout.
- The Windows test build (CI) no longer fails: three tests used Unix-only calls.

### Added
- **The version is shown at the bottom of Settings** ("NyForge v0.1.8").

## 0.1.7: the huge update

This is a **huge update**: the biggest one so far. It adds **one new optional tool, NyTest** (off until you switch it on in Settings), and touches the other three, the boot menu and the look of the app.

### NyTest (new)
- **Speed test like CrystalDiskMark** (sequential 1 MiB at queue depth 8 and 1, random 4 KiB at queue depth 32 and 1), measured the careful way (uncached I/O, incompressible random data, flush counted, median of several runs) and with **IOPS, latency percentiles and run-to-run spread** on top.
- **Sustained write** with a speed graph and the point where a fast cache runs out marked.
- **Surface scan** with a coloured map of slow and unreadable areas; **capacity check** that finds fake-capacity drives and failing flash.
- **The drive and its link:** USB speed (and why a fast stick can look slow), UASP, SD and eMMC data, an estimated SD speed class, and the drive's own health report when it has one: SMART health, hours powered on, temperature, wear and bad-sector counters (needs `smartctl`; most flash sticks and SD cards have no such counters, and NyTest says so).
- **Removable drives only:** NyTest lists and accepts only USB sticks, SD cards and other removable drives. Internal and system disks are refused.
- A verdict in plain words that never claims more than was tested; copy or save the report as text or JSON. Basic (Quick, Standard, Full) and advanced settings. Nothing is written to the drive unless erasing is allowed; speed tests use a temporary file that is removed afterwards.
- `nyforge test` on the command line.

### NyBoot
- **New graphical boot menu** (BIOS and UEFI): one flat grey page, a plain list of your images, key hints in the bottom-left corner. Square corners, Liberation Sans, no pictures. Text size follows the screen (three sizes for 1080p, 1440p and 4K).
- **Custom themes, like Ventoy.** Any GRUB 2 theme works, the same ones Ventoy uses. Copy a theme folder onto the drive with `nyforge boot theme <drive> <folder>` (or put it in `/nyboot/themes/<name>/` yourself) and pick it under **Menu settings → Theme**, or with `nyforge boot config <drive> --theme <name>`. If the theme is missing the built-in one is used. A light example is in `docs/themes/light`.
- **Memtest86+ 7.00** is built in (BIOS and UEFI), next to *UEFI Firmware Settings* and *Shutdown*.
- **Update bootloader**: new boot code and menu on an existing drive, your images and settings stay.
- **Menu settings** (timeout, preselected image, plain text mode, folder depth, theme) and per-image **boot options** such as `nomodeset`.

### NyFlash and Windows media
- Windows ISOs are recognised properly (including NTFS layouts) and the install answer-file options were extended.
- Faster writing and verifying.
- On Windows a notification appears at the corner of the screen when a long job finishes while NyForge is in the background.

### NyFormat
- Multi-pass wipe with read-back check, and the drive can be ejected when it is done.
- A shorter, clearer description on the home screen.

### Platforms
- **macOS is not available.** NyForge is for Linux and Windows. There is no macOS download, and the automatic tests and releases no longer build for it.

### The app
- **Settings** behind a small cog at the top right of the window: put the tools of the start screen in your own order (drag them, or use the arrows) and switch the optional **NyTest** on. NyTest is off by default, so the start screen shows NyFlash, NyBoot and NyFormat; once it is on it is the fourth tool. Everything is saved automatically.
- **One start screen, no pages.**
- New look and logo, smoother animations, better scaling.
- **Fixed choppy animations.** The frame limiter counted its own sleep as work, so after the first animation it slowed the app down to about 30 frames a second on any computer. Hovering and moving now run at the full speed of the screen again (up to about 120 frames a second).
- Ctrl+1 to 3 open the tools (Ctrl+4 opens NyTest once it is on). NyTest uses the same calm grey and blue look as the rest of the app. Hovering a tool card lights its ring in a lighter grey and fills the **Open** button blue; every button and every selection is blue.

### Not tested yet
**NyTest** was tested on Linux only: with loop devices and image files, but not yet on a real fake-capacity stick, an SD card reader or a USB SSD with SMART, and the `smartctl` reader has not met every kind of drive. They are not yet tried on Windows. Real hardware with **Secure Boot** on (Memtest86+ is not signed), and the Windows answer files and ejecting on real machines. Tested so far: QEMU (BIOS and UEFI), Windows 11 and Tiny10 laptops, CachyOS.
