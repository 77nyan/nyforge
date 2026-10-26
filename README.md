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
| **NyBoot** | Multiboot drive: install once, copy ISOs onto it like files, pick one at boot. Not Ventoy-compatible; no Windows ISOs. | `nyforge boot` |
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

### Linux and macOS

In the project (or unpacked download) folder:

```sh
./install.sh --user      # for you only, no password, installs to ~/.local (recommended)
sudo ./install.sh        # for everyone on this computer
```

From source it builds first (needs [Rust](https://rustup.rs) 1.88+, Git and a C compiler; a few minutes). Then open
**NyForge** from the applications menu or run `nyforge-gui`. Remove it with `nyforge-uninstall` (installed next to the
program; for an all-users copy use its full path with `sudo`).

* **No menu entry after `--user`**: put `~/.local/bin` on your `PATH`, then log out and in once.
* **Rust missing under `sudo`**: use `--user`. The installer builds as you and only installs as root.
* **macOS**: the app is not signed yet; the installer clears the quarantine mark (by hand: `xattr -dr
  com.apple.quarantine NyForge.app`, or right-click > **Open**). If writing says "Operation not permitted", allow
  NyForge under Privacy & Security > Full Disk Access.

### Windows

1. Download `nyforge-<version>-windows-x86_64.zip` from **Releases** and **Extract All**.
2. Add `C:\Users\<you>\AppData\Local\Programs\NyForge` to Windows Defender's exclusions, or it may block NyForge's
   drive access. In PowerShell as administrator:
   `Add-MpPreference -ExclusionPath "$env:LOCALAPPDATA\Programs\NyForge"`.
3. Double-click **`install.cmd`** and open **NyForge** from the Start menu. Remove it with *Settings > Apps > NyForge*
   or `uninstall.cmd` in the install folder.

Writing to a drive needs administrator rights; the app offers **Restart as administrator**. The program is not
code-signed, so SmartScreen may warn: **More info > Run anyway**. To build from source instead, run `build-windows.cmd`
(needs Rust).

> **Windows ISOs, please read.**
> * NyForge's Windows mode makes a bootable installer from a Windows ISO and is meant mainly for custom builds such as
>   Tiny10 and Tiny11. For an official Windows install use Microsoft's **Media Creation Tool**.
> * **Big Windows installs can fail.** The install image must fit a FAT32 drive: a normal `install.wim` over 4 GB is
>   split automatically, but an `install.esd` or compact image over 4 GB cannot be written (the app warns first).
> * **Windows ISOs do not work with NyBoot** (custom ones neither): a Windows installer cannot start from a multiboot
>   drive.
> * Not yet tried on a real Windows Setup boot.

### Downloads (Releases page)

| System | File |
|---|---|
| Linux x86-64 (glibc 2.35+) | `nyforge-<version>-linux-x86_64.tar.gz` (app + command line) |
| macOS 11+ (Apple Silicon and Intel) | `nyforge-<version>-macos-universal.tar.gz` |
| Windows 10/11 x86-64 | `nyforge-<version>-windows-x86_64.zip` |
| Older Linux, ARM64, Alpine | `nyforge-cli-<version>-linux-<arch>[-musl].tar.gz` (command line only) |

Each has a `.sha256` file (`sha256sum -c`, `shasum -a 256 -c`, or `Get-FileHash`).

### What Linux needs

Almost nothing; most desktops have it. On Arch / Omarchy:

```sh
sudo pacman -S --needed mesa libxkbcommon wayland libx11 polkit xdg-desktop-portal xdg-desktop-portal-hyprland e2fsprogs ntfs-3g
```

Debian/Ubuntu: `libgl1 libxkbcommon0 libwayland-client0 libx11-6 policykit-1 xdg-desktop-portal` plus a portal backend.
Fedora: `mesa-libGL libxkbcommon wayland-libs-client libX11 polkit xdg-desktop-portal`. `e2fsprogs` and `ntfs-3g` are
optional (ext2/3/4 and NTFS formatting). Writing asks for your password through polkit; the app never runs as root.

## Using the app

* **NyFlash**: choose an image (button or drag and drop), choose a drive, **Flash...**, confirm. Real progress, safe
  stop. No drive is ever pre-selected.
  * **Windows ISO:** recognised automatically and turned into a bootable installer (UEFI with Secure Boot, and legacy
    BIOS). Options: skip the Windows 11 hardware checks, legacy BIOS on/off. A build with its own `autounattend.xml`
    keeps it. "Write the ISO as it is" is still there. See the note under **Windows** above.
  * **Verify (optional):** tick *Verify when done* before or while it runs, or press *Verify drive* afterwards. It
    reads the drive back and compares it with the image (a Windows installer: every file and the boot sector). A
    failure names the first wrong byte or file.
  * **Checksum:** the *Checksum* link checks a download against a SHA-256, SHA-1 or MD5. A `SHA256SUMS` or
    `<file>.sha256` next to it is used automatically.
  * On Windows, writes go in 1 MiB pieces; if the stick drops out and comes back as the very same drive, the write
    carries on (up to three times). A different drive is never written to.
* **NyBoot**: choose a drive, install NyBoot (erases it), then **Add images**, or copy `.iso` files onto the big
  `NYBOOT` (exFAT) partition with your file manager. They show up in the boot menu next start. Images are checked when
  added. Unknown ISOs get a generic start (kernel + initrd found on the image) or use their own `grub.cfg`. For an
  image's own menu put `<image>.iso.nyboot.cfg` next to it with `set nb_profile="loopbackcfg"`; the same file can add
  options, e.g. `set nb_params="nomodeset"`.
* **NyFormat**: choose a drive, a filesystem (only ones this system can make; an **i** button explains a greyed-out
  one), a label and cluster size, then **Format!**. Options: **Quick format**, **Check device for bad blocks**, and
  **Restore full capacity** for a drive that shows only a few MB after flashing an ISO.
* Closing the window during a write asks first, then stops it. Progress and errors are the engine's own; "Verified"
  appears only when the drive really was read back.

## Command line

Everything the app does is `nyforge`, plus Raspberry Pi first-boot setup and drive cloning. Add `--json` for one JSON
event per line (how the app talks to the engine). `nyforge list` shows drives. Use `sudo` on Linux/macOS, an
Administrator terminal on Windows.

```sh
nyforge list                                   # drives you can write to
nyforge inspect ubuntu.iso                     # what is this image?
sudo nyforge flash ubuntu.iso /dev/sdb --verify   # write it (asks first), then read it back
sudo nyforge flash raspios.img.xz /dev/sdb --hostname mypi --user me --ssh --ssh-key ~/.ssh/id_ed25519.pub
sudo nyforge verify ubuntu.iso /dev/sdb         # check a drive later
nyforge checksum ubuntu.iso --expect SHA256SUMS # or a hash; with neither, a checksum file beside it is used
sudo nyforge windows Win11.iso /dev/sdb --bypass-checks --verify
sudo nyforge boot install /dev/sdb             # multiboot drive
sudo nyforge boot add /dev/sdb *.iso
sudo nyforge format /dev/sdb --fs exfat -l HOLIDAY
sudo nyforge format /dev/sdb --check-bad-blocks --passes 2
nyforge clone /dev/sdb backup.img.zst
nyforge doctor                                 # does this machine have what NyForge needs?
```

`-y/--yes` skips confirmation. Writing to a file path never needs root. Exit codes: 0 success, 1 failure, 2 bad usage,
130 cancelled.

## Build and test

`cargo build --release -p nyforge -p nyforge-gui`, then `cargo test -p nyforge-core`. The QEMU boot tests need
`qemu-system-x86_64`, OVMF, `parted`, `mtools`, `dosfstools`, `grub`, `wimlib`, `xorriso`; see `tools/` and run
`cargo test -p nyforge-core --test nyboot_qemu --test winmedia_qemu -- --ignored --test-threads=1`.

## What has been tested

* **Real hardware (Linux and Windows 11):** flashing (including Alpine Extended), NyBoot, formatting and the app, used
  by the author on real USB sticks.
* **Automatic tests (Linux):** writing, read-back verification (a flipped bit anywhere is found, also in a Windows
  installer), checksums, cancellation, decompression, ISO readers, `install.wim` handling, `nyforge format` (checked
  with `fsck`, `parted`, `blkid`), QEMU boots of NyBoot and of a Windows-style stick (BIOS and UEFI), and the app under
  a virtual display.
* **On GitHub (Windows, macOS, Linux ARM64/musl):** builds, unit tests, drive detection, flashing and reading back a
  virtual disk.
* **Not tested by hand:** the macOS app; a real Windows Setup boot; Raspberry Pi first boot; NyBoot with real ISOs of
  every distro; the capacity test on a counterfeit stick.

## Known limits

* NyBoot: no Secure Boot, no persistence, MBR only (drives up to 2 TiB), no in-place update, no Windows ISOs.
* Windows installer: an install image over 4 GB that cannot be split is unsupported. Verify checks the files, boot
  sector and GRUB code, not what Windows' boot manager does with them.
* ext2/3/4 and NTFS formatting need Linux with `mkfs.ext*` / `mkfs.ntfs`; on Windows NTFS is made by Windows; macOS can
  make neither. Drives over 2 TB default to GPT.
* Windows 10 or newer; no Windows ARM64. macOS and Windows builds are not code-signed.

## Contributing and publishing

Maintainers: [docs/PUBLISHING.md](docs/PUBLISHING.md).

## License

GPL-3.0-or-later. See `LICENSE` and `THIRD-PARTY.md`.
