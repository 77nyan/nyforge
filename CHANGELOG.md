# Changelog

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

### The app
- **Settings** behind a small cog at the top right of the window: put the tools of the start screen in your own order (drag them, or use the arrows) and switch the optional **NyTest** on. NyTest is off by default, so the start screen shows NyFlash, NyBoot and NyFormat; once it is on it is the fourth tool. Everything is saved automatically.
- **One start screen, no pages.**
- New look and logo, smoother animations, better scaling.
- Ctrl+1 to 3 open the tools (Ctrl+4 opens NyTest once it is on). NyTest uses the same calm grey and blue look as the rest of the app. Hovering a tool card lights its ring in a lighter grey and fills the **Open** button blue; every button and every selection is blue.

### Not tested yet
**NyTest** was tested on Linux only: with loop devices and image files, but not yet on a real fake-capacity stick, an SD card reader or a USB SSD with SMART, and the `smartctl` reader has not met every kind of drive. They are not yet tried on Windows or macOS. Real hardware with **Secure Boot** on (Memtest86+ is not signed), and the Windows answer files, macOS and ejecting on real machines. Tested so far: QEMU (BIOS and UEFI), Windows 11 and Tiny10 laptops, CachyOS.
