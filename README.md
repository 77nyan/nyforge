<p align="center"><img src="assets/logo/nyforge-128.png" alt="NyForge logo" width="96"></p>

<h1 align="center">NyForge</h1>

<p align="center">
  A fast, small, native USB toolkit for Linux, macOS and Windows.<br>
  Flash images, build a multiboot drive, format USB sticks. Rust, no telemetry, no account.
</p>

<p align="center">
  <img src="docs/screenshots/home.png" alt="NyForge home screen" width="480">
  <img src="docs/screenshots/flash.png" alt="NyFlash screen" width="480">
</p>

| Tool | What it does | Command |
|---|---|---|
| **NyFlash** | Writes `.iso` / `.img` files (and `.gz` `.xz` `.bz2` `.zst` `.zip` archives of them) to a USB drive. Turns a Windows ISO into a bootable installer. Can verify the result and check a download's checksum. | `nyforge flash`, `windows`, `verify`, `checksum` |
| **NyBoot** | Multiboot drive: install once, copy ISOs onto it like files, pick one at boot. Not Ventoy-compatible. | `nyforge boot` |
| **NyFormat** | Erases and formats USB sticks and SD cards (FAT32, exFAT, FAT16, NTFS, ext2/3/4 where the system allows), checks for bad blocks and fake capacity, restores a drive stuck at a few MB. | `nyforge format` |

A desktop app (`nyforge-gui`) and a command line (`nyforge`) share one engine; the app never touches a drive itself.
Keys: **Ctrl+1/2/3** (**Cmd** on macOS) open the tools, **Alt+Left** (**Cmd+[**) goes back, **F11** full screen.
The window shrinks to a quarter-screen tile, so it works in tiling window managers such as Hyprland.

**Safety:** the disk your system runs from is never listed, and NyFormat lists only removable USB drives and SD cards.
System, internal and virtual disks are refused by the engine, with no override. Every write asks first and re-checks
the drive right before starting.

> **Young software.** Tested on Linux and Windows 11 on real hardware; **not tested on macOS** (only built and
> checked by automatic tests). Read [What has been tested](#what-has-been-tested) before trusting it with a drive
> that holds anything you care about.

## Install

### Linux and macOS: `./install.sh`

Open a terminal in the project (or unpacked download) folder and run **one** of these:

```sh
./install.sh --user      # for you only, no password, installs to ~/.local (recommended)
sudo ./install.sh        # for everyone on this computer
```

From source (git clone or `nyforge-source.tar.gz`) it builds first, which needs [Rust](https://rustup.rs) 1.88+, Git
and a C compiler, and takes a few minutes. Then open **NyForge** from the applications menu or run `nyforge-gui`.
Check with `nyforge --version`. To remove it, run **`nyforge-uninstall`** (the installer puts it next to the program;
`sudo nyforge-uninstall` also removes an all-users copy).

* **"Permission denied"**: you ran it without `--user` or `sudo`. Add one.
* **"command not found"**: wrong folder. `cd` to the folder that holds `install.sh`.
* **Rust missing under `sudo`**: use `--user`, or run `rustup default stable` for your own account. The installer
  builds as you and only installs as root.
* **No menu entry after `--user`**: put `~/.local/bin` on your `PATH`, then log out and in once.
* **macOS**: `--user` puts `NyForge.app` in `~/Applications` (`sudo` uses `/Applications`). The app is not signed
  yet; the installer clears the quarantine mark. If you copied it by hand: `xattr -dr com.apple.quarantine NyForge.app`
  or right-click > **Open**. If macOS says "Operation not permitted" when writing, allow NyForge under Privacy &
  Security > Full Disk Access (or Removable Volumes).

### Windows

1. Download `nyforge-<version>-windows-x86_64.zip` from **Releases** and **Extract All**.
2. **Add the install folder to Windows Defender's exclusions** (see below), or Defender may delete or block NyForge's
   drive access. The folder is `C:\Users\<you>\AppData\Local\Programs\NyForge` (the installer prints it).
3. Double-click **`install.cmd`** and open **NyForge** from the Start menu. To remove it: *Settings > Apps > NyForge >
   Uninstall* (or run `uninstall.cmd` in the install folder).

Writing to a drive needs administrator rights; the app offers **Restart as administrator** (UAC prompt). The program is
not code-signed yet, so SmartScreen may warn: **More info > Run anyway**.

**Defender exclusion:** Start > *Windows Security* > *Virus & threat protection* > *Manage settings* > *Exclusions* >
*Add or remove exclusions* > *Add an exclusion* > *Folder*, then pick the folder above. Or, in PowerShell as
administrator: `Add-MpPreference -ExclusionPath "$env:LOCALAPPDATA\Programs\NyForge"`.

### Downloads (Releases page)

| System | File |
|---|---|
| Linux x86-64 (glibc 2.35+) | `nyforge-<version>-linux-x86_64.tar.gz` (app + command line) |
| macOS 11+ (Apple Silicon and Intel) | `nyforge-<version>-macos-universal.tar.gz` |
| Windows 10/11 x86-64 | `nyforge-<version>-windows-x86_64.zip` |
| Older Linux, ARM64, Alpine | `nyforge-cli-<version>-linux-<arch>[-musl].tar.gz` (command line only; build from source for the app) |

Each has a `.sha256` file: `sha256sum -c <file>.sha256` (Linux), `shasum -a 256 -c <file>.sha256` (macOS),
`Get-FileHash <file>` (Windows). Unpack, `cd` into the folder, run `./install.sh --user`.

### What Linux needs

Almost nothing: NyForge has its own FAT32/exFAT code, and most desktops already have the rest. On Arch / Omarchy:

```sh
sudo pacman -S --needed mesa libxkbcommon wayland libx11 polkit xdg-desktop-portal xdg-desktop-portal-hyprland e2fsprogs ntfs-3g
```

Debian/Ubuntu: `libgl1 libxkbcommon0 libwayland-client0 libx11-6 policykit-1 xdg-desktop-portal` (plus a portal backend:
`-gtk`, `-gnome`, `-kde` or `-hyprland`). Fedora: `mesa-libGL libxkbcommon wayland-libs-client libX11 polkit
xdg-desktop-portal`. `e2fsprogs` (ext2/3/4) and `ntfs-3g` (NTFS) are optional: without them the app does not offer
those. Building needs `rustup base-devel git`. Writing asks for your password through polkit; the app never runs as
root. Manual build: `cargo build --release -p nyforge -p nyforge-gui`; keep both programs in one folder.

## Using the app

* **NyFlash**: choose an image (button or drag and drop), choose a drive, **Flash...**, confirm. Real progress, safe stop.
  No drive is ever pre-selected.
  * **Windows ISO:** recognised automatically. NyForge builds a bootable installer instead of copying the ISO as it is
    (that would not boot): UEFI (Secure Boot too) and legacy BIOS, an `install.wim` too big for FAT32 is split for you.
    Options: skip the Windows 11 hardware checks, legacy BIOS on/off. "Write the ISO as it is" is still there.
    This mode is mainly meant for modified Windows builds such as Tiny10 and Tiny11. For an official Windows install,
    use Microsoft's own Media Creation Tool; it is the supported way and gets you the newest version.
    A build that brings its own `autounattend.xml` keeps it. An `install.esd` over 4 GB cannot go on a FAT32 drive
    (it cannot be split); the app says so before you start.
  * **Windows writes** go in 1 MiB pieces, and if the stick drops out for a moment and comes back as the very same
    drive, the write carries on (up to three times). A different drive is never written to.
  * **Verify (optional, never in the way):** tick *Verify when done* before or while it runs, or press *Verify drive*
    on the result screen. It reads the drive back, bypassing the system cache, and compares every byte with the image
    (for a Windows installer: every file, and the boot sector). A failure names the first wrong byte or file.
  * **Checksum:** the *Checksum* link checks the download against the SHA-256, SHA-1 or MD5 on its web page. A
    `SHA256SUMS` or `<file>.sha256` next to it is found and used.
* **NyBoot**: choose a drive, install NyBoot (erases it), then **Add images**. Removing an image deletes only that file;
  **Reinstall** erases the drive. "Update bootloader" is disabled (no in-place update yet). Images are checked when
  added; Windows installers cannot boot from a loop device and NyForge says so.
  * A drive with `NYBOOT` / `NYBOOT-EFI` partitions is marked "NyBoot drive" and chosen for you when it is the only one.
  * **No app needed to add images:** the big `NYBOOT` partition is plain exFAT. Copy `.iso` files onto it with your file
    manager, in any folder; they appear in the boot menu next start (tested in an emulator, BIOS and UEFI).
  * **Unknown ISOs:** no per-distro recipe needed; unknown images get a generic start (kernel + initrd found on the
    image) and images with their own `grub.cfg` can use it. That is how distros Ventoy-style tools struggle with, such
    as CachyOS, are meant to work (unproven on real ISOs).
  * Arch-family images are started by NyBoot itself (their own menu searches every disk first and is slow). For an
    image's own menu put `<image>.iso.nyboot.cfg` next to it with `set nb_profile="loopbackcfg"`; the same file can add
    options, e.g. `set nb_params="nomodeset"`.
* **NyFormat**: choose a drive, a filesystem (only ones this system can make), a label and cluster size, then
  **Format!**. A greyed-out filesystem has an **i** button that explains why and how to fix it on your system. Options: **Quick format**, **Check device for bad blocks** (1 to 4 passes), and **Restore full capacity**
  for a drive that shows only a few MB after flashing an ISO. A cancelled flash leaves such a drive; this fixes it.
* Closing the window during a write asks first, then stops it. Progress and errors are the engine's own; nothing is
  simulated. "Verified" appears only when the drive really was read back. Animations follow "reduce motion".

## Command line

Everything the app does is `nyforge`, plus Windows installer media, Raspberry Pi first-boot setup and drive cloning.
Add `--json` for one JSON event per line (how the app talks to the engine). Drives are named as your system names
them (`/dev/sdb`, `/dev/disk4`, `\\.\PhysicalDrive2`); `nyforge list` shows them. Use `sudo` on Linux/macOS, an
Administrator terminal on Windows.

```sh
nyforge list                                   # drives you can write to
nyforge inspect ubuntu.iso                     # what is this image?
sudo nyforge flash ubuntu.iso /dev/sdb         # write it (asks first)
sudo nyforge flash raspios.img.xz /dev/sdb /dev/sdc --hostname mypi --user me --ssh --ssh-key ~/.ssh/id_ed25519.pub
sudo nyforge flash ubuntu.iso /dev/sdb --verify   # then read the drive back and compare
sudo nyforge verify ubuntu.iso /dev/sdb         # check a drive later
nyforge checksum ubuntu.iso --expect SHA256SUMS # or --expect <the hash>; with neither, a checksum file beside it is used
sudo nyforge windows Win11.iso /dev/sdb --bypass-checks --verify
sudo nyforge verify Win11.iso /dev/sdb --windows --bypass-checks
sudo nyforge boot install /dev/sdb             # multiboot drive
sudo nyforge boot add /dev/sdb *.iso
nyforge boot check some.iso
sudo nyforge format /dev/sdb --fs exfat -l HOLIDAY
sudo nyforge format /dev/sdb --scheme gpt --partition fat32:16G:WIN --partition ext4::LINUX
sudo nyforge format /dev/sdb --check-bad-blocks --passes 2         # write and read back the whole drive twice (finds bad blocks and fakes)
sudo nyforge format /dev/sdb --test-capacity full --wipe zero      # unmask fake capacity, then erase
nyforge format --list                          # what could be formatted (never internal disks)
nyforge clone /dev/sdb backup.img.zst
nyforge doctor                                 # does this machine have what NyForge needs?
```

`-y/--yes` skips confirmation. Writing to a file path (`nyforge flash x.iso out.img`) never needs root.
Exit codes: 0 success, 1 failure, 2 bad usage, 130 cancelled.

## Build and test

`cargo build --release -p nyforge -p nyforge-gui`, then `cargo test -p nyforge-core`. The QEMU boot tests need
`qemu-system-x86_64`, OVMF, `parted`, `mtools`, `dosfstools`, `grub`, `wimlib`, `xorriso` and are run with
`cargo test -p nyforge-core --test nyboot_qemu --test winmedia_qemu -- --ignored --test-threads=1`
(`tools/make-test-kernel.sh <dir>` builds the kernel; set `NYFORGE_TEST_KERNEL_DIR`). GRUB code in `assets/grub` is
prebuilt by `tools/build-grub-assets.sh`.

## What has been tested

* **Linux (x86-64) and Windows 11, real hardware:** flashing, NyBoot, formatting and the desktop app, used by the author on
  real USB sticks. A Windows bug found this way (a cancelled flash broke later writes) is fixed in 0.1.2.
* **Automatic tests (Linux):** writing, read-back verification (a single flipped bit anywhere is found, also in a
  Windows installer's files and boot sector), checksums, cancellation, decompression, ISO readers, `install.wim` splitting, first-boot
  files, `nyforge format` (FAT16/32, exFAT, NTFS, ext2/3/4 checked with `fsck`/`e2fsck`/`ntfsfix`/`blkid`; MBR and GPT
  checked with `parted`; fake-capacity detection), QEMU boots of NyBoot (real kernel; ISOs copied in by a file manager
  and by NyForge; BIOS and UEFI) and of a Windows-style stick, and the app under a virtual display.
* **Automatic tests on GitHub (Windows, macOS, Linux ARM64/musl):** builds, unit tests, drive detection on real
  machines, and flashing then reading back a virtual disk (and a RAM disk on macOS).
* **Not tested by hand:** the macOS app; a real Windows Setup boot; Raspberry Pi first boot on a Pi; NyBoot with real
  ISOs of every distro (profiles for Debian-live, Arch family incl. CachyOS, Manjaro, Fedora-style, own-`grub.cfg`
  images and a generic fallback are checked against ISO layouts in an emulator); `nyforge format` partitions on a real
  Linux drive; the quick capacity test on a counterfeit stick.

## Known limits

* NyBoot: no Secure Boot, no persistence, MBR only (drives up to 2 TiB, 512-byte sectors), no in-place update;
  `install.esd` over 4 GiB is unsupported.
* Verifying is optional and off by default; it takes about as long as the write. A Windows installer's check covers the
  files copied from the ISO, the boot sector and GRUB's code, not what Windows' boot manager does with them.
* `nyforge format` makes FAT16/FAT32/exFAT itself everywhere. ext2/3/4 and NTFS need Linux with `mkfs.ext*` / `mkfs.ntfs`
  (`nyforge doctor` checks); on Windows NTFS is made by Windows; macOS can make neither. Drives over 2 TB default to GPT.
* Windows 10 or newer. The macOS download is one universal build (its Intel half is only cross-compiled). No Windows ARM64.
* macOS and Windows builds are not code-signed or notarized.

## Contributing and publishing

Maintainers: [docs/PUBLISHING.md](docs/PUBLISHING.md) covers putting the repository on GitHub and publishing a release.

## License

GPL-3.0-or-later. See `LICENSE` and `THIRD-PARTY.md`.
