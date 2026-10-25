<p align="center">
  <img src="assets/logo/nyforge-128.png" alt="NyForge logo" width="96">
</p>

<h1 align="center">NyForge</h1>

<p align="center">
  A fast, small, native USB toolkit for Linux, macOS and Windows.<br>
  Flash images, build a multiboot drive, format USB sticks. Written in Rust, no telemetry, no account.
</p>

<p align="center">
  <img src="docs/screenshots/home.png" alt="NyForge home screen with NyFlash, NyBoot and NyFormat" width="620">
</p>

<p align="center">
  <img src="docs/screenshots/flash.png" alt="NyFlash: choose an image, choose a drive, flash" width="620">
</p>

NyForge has a desktop app (`nyforge-gui`) and a command line (`nyforge`). They share one engine, and the
app never touches a drive itself: it starts the engine and shows its real progress.

Keyboard: **Ctrl+1 / 2 / 3** (**Cmd** on macOS) open NyFlash / NyBoot / NyFormat, **Alt+Left** (**Cmd+[** on macOS) goes back, **F11** toggles full screen.
The window scales down to a quarter-screen tile, so it works in tiling window managers such as Hyprland.

| Tool | What it does | Command line |
|---|---|---|
| **NyFlash** | Writes `.iso` / `.img` files (and `.gz` `.xz` `.bz2` `.zst` `.zip` archives of them) to a USB drive. | `nyforge flash` |
| **NyBoot** | A multiboot drive: install once, copy ISOs onto it like files, pick one at boot. Not Ventoy, not Ventoy-compatible. | `nyforge boot` |
| **NyFormat** | Erases and formats USB sticks and SD cards (FAT32, exFAT, FAT16, ext4, NTFS where the system allows it). | `nyforge format` |

Safety first: NyForge never lists the disk your system runs from, and **NyFormat only ever lists removable
USB drives and SD cards**. System disks, internal disks and virtual devices are refused by the engine itself
and there is no setting to override that. Every write asks for confirmation and checks the drive again
right before it starts.

> Status: **version 0.1, young software.** Read [What has been tested](#what-has-been-tested) before trusting
> it with a drive that holds anything you care about. Linux is the system that has been exercised the most.

## Download and install

### Quick install (Linux and macOS): `./install.sh`

The installer is the way to install NyForge. It is in the main folder of the project and of every Linux and macOS
download. Open a terminal in that folder and run **one** of these:

```sh
./install.sh --user      # for you only: no password, installs to ~/.local  (recommended)
sudo ./install.sh        # for everyone on this computer
```

If you downloaded the source (a git clone or `nyforge-source.tar.gz`) the installer builds the program first, which
needs [Rust](https://rustup.rs) and takes a few minutes. Then open **NyForge** from your applications menu or run
`nyforge-gui`. Check what you installed with `nyforge --version`, and remove it with `./install.sh --user --uninstall`
(or `sudo ./install.sh --uninstall`).

Common mistakes:

* **"Permission denied" while installing**: you ran `./install.sh` without `--user` and without `sudo`. Add one of them.
* **"command not found"**: you are in the wrong folder. `cd` into the folder that contains `install.sh` (check with `ls install.sh`).
* **Rust missing under `sudo`**: use `./install.sh --user`, or install Rust for your own account (`rustup default stable`);
  the installer builds as you and only installs as root.

### Other downloads

Get the file for your system from the **Releases** page of this repository (the link is on the right of the
GitHub page), then follow the steps for your system.

| System | File | What is in it |
|---|---|---|
| Linux (x86-64) | `nyforge-<version>-linux-x86_64.tar.gz` | App + command line. Needs glibc 2.35 or newer (Ubuntu 22.04+, Debian 12+, Fedora 36+, Arch, and similar). |
| macOS 11+ (Apple Silicon and Intel) | `nyforge-<version>-macos-universal.tar.gz` | `NyForge.app` + command line. |
| Windows 10 / 11 (x86-64) | `nyforge-<version>-windows-x86_64.zip` | `nyforge-gui.exe` (the app) + `nyforge.exe` (command line). |

Older Linux distros, ARM64 and Alpine: `nyforge-cli-<version>-linux-<arch>[-musl].tar.gz` is the command line
only, built to run on glibc 2.17 and newer or fully static. For the app on those systems, build from source (below).

Every download has a `.sha256` file next to it. To check a download, run `sha256sum -c <file>.sha256` (Linux),
`shasum -a 256 -c <file>.sha256` (macOS), or compare with `Get-FileHash <file>` (Windows).

### Linux

1. Download the `.tar.gz` file (it will land in your Downloads folder).
2. Open a terminal and run:

   ```sh
   cd ~/Downloads
   tar xzf nyforge-0.1.0-linux-x86_64.tar.gz
   cd nyforge-0.1.0-linux-x86_64
   ./install.sh --user        # only for you, no password needed (recommended)
   # or: sudo ./install.sh    # for everyone on this computer
   ```

3. Open **NyForge** from your applications menu, or type `nyforge-gui`.

When you flash or format, the app asks for your password through your system's normal authorization window
(polkit). The app itself never runs as root. The file chooser needs `xdg-desktop-portal` and a backend for your
desktop (`xdg-desktop-portal-gtk`, `-gnome`, `-kde` or `-hyprland`), which almost every desktop already has.
You can also drag an image onto the NyFlash window. Not seeing the menu entry after `--user`? Make sure
`~/.local/bin` is on your `PATH` and log out and in once.

Remove it with `sudo ./install.sh --uninstall` (or `./install.sh --user --uninstall`).

#### What Linux needs

NyForge brings its own code for everything else (including FAT32 and exFAT), so there is very little to install.
Almost every desktop already has all of this:

| Needed for | Package (Arch / Omarchy) | Debian / Ubuntu | Fedora |
|---|---|---|---|
| The app window (OpenGL and Wayland or X11) | `mesa libxkbcommon wayland libx11` | `libgl1 libxkbcommon0 libwayland-client0 libx11-6` | `mesa-libGL libxkbcommon wayland-libs-client libX11` |
| The password window when flashing or formatting | `polkit` (plus a polkit agent, which desktops start for you) | `policykit-1` | `polkit` |
| The file chooser | `xdg-desktop-portal` and one backend (`-gtk`, `-gnome`, `-kde` or `-hyprland`) | same names | same names |
| Formatting a drive as **ext4** (optional) | `e2fsprogs` | `e2fsprogs` | `e2fsprogs` |
| Formatting a drive as **NTFS** (optional) | `ntfs-3g` | `ntfs-3g` | `ntfs-3g` |

One command for Arch and Omarchy (anything already installed is skipped):

```sh
sudo pacman -S --needed mesa libxkbcommon wayland libx11 polkit xdg-desktop-portal xdg-desktop-portal-hyprland e2fsprogs ntfs-3g
```

Without `e2fsprogs` or `ntfs-3g` the app simply does not offer ext4 or NTFS. To build from source you also need
Rust and a C compiler (`sudo pacman -S --needed rustup base-devel git && rustup default stable`).

### macOS

1. Download the `.tar.gz` file and double-click it in Finder to unpack it (or `tar xzf` in Terminal).
2. Open Terminal in the unpacked folder and run:

   ```sh
   cd ~/Downloads/nyforge-0.1.0-macos-universal
   ./install.sh --user        # puts NyForge.app in ~/Applications; use sudo ./install.sh for /Applications
   ```

3. Open **NyForge** from Launchpad or Applications.

The app is **not signed or notarized yet**, so macOS may say it "cannot be opened". The installer already clears
that mark. If you copied `NyForge.app` by hand, run `xattr -dr com.apple.quarantine NyForge.app` once, or right-click
the app and choose **Open**. When you flash or format, macOS shows its own administrator password window. If macOS
answers "Operation not permitted" when writing to a drive, allow NyForge under System Settings > Privacy & Security
> Full Disk Access (or Removable Volumes).

### Windows

1. Download the `.zip` file, right-click it and choose **Extract All**.
2. Open the extracted folder, right-click `install.ps1` and choose **Run with PowerShell**.
   (If Windows blocks it, open PowerShell in that folder and run `powershell -ExecutionPolicy Bypass -File .\install.ps1`.)
3. Open **NyForge** from the Start menu.

Writing to a drive needs administrator rights on Windows. If NyForge is not running as administrator it says so and
offers **Restart as administrator**, which shows the normal Windows (UAC) prompt. The program is **not code-signed
yet**, so SmartScreen may say "unknown publisher": click **More info**, then **Run anyway**.
Remove it with `.\install.ps1 -Uninstall`.

### Build from source (any system)

You need [Rust](https://rustup.rs) 1.88 or newer and Git. On Linux also install a C compiler (`build-essential`,
`base-devel`, or your distro's equivalent).

```sh
git clone https://github.com/77nyan/nyforge
cd nyforge
cargo build --release -p nyforge -p nyforge-gui
./target/release/nyforge-gui          # the app (Windows: target\release\nyforge-gui.exe)
./install.sh --user                   # Linux and macOS: install it for yourself (builds first if needed); or: sudo ./install.sh
```

`77nyan` is the GitHub account that hosts this repository. The first build takes a few minutes.
Keep `nyforge` and `nyforge-gui` in the same folder: the app looks for the engine next to itself.

## Using the app

* **Home** shows the three tools. Every tool has a **Home** button at the top left.
* **NyFlash**: choose an image (button or drag and drop), choose a drive, press **Flash...**, confirm. You see real
  progress (percent, bytes, speed) and can stop safely. A drive is never pre-selected.
* **NyBoot**: choose a drive, install NyBoot (this erases the drive), then **Add images**. Removing an image only deletes that
  file; **Reinstall** is a separate, clearly marked action that erases the drive. "Update bootloader" is shown but
  disabled, because the engine cannot update in place yet. Each image is checked when you add it, and some (for
  example Windows installers) cannot boot from a multiboot drive. NyForge tells you instead of pretending.
  NyForge recognises a NyBoot drive by its `NYBOOT` / `NYBOOT-EFI` partitions: it is marked "NyBoot drive" in the drive
  list and chosen for you when it is the only one plugged in.
  **You do not need the app to add images.** The big `NYBOOT` partition is a normal exFAT drive that Windows, macOS and
  Linux all open: copy `.iso` files onto it with your file manager, in any folder, and they appear in the boot menu at
  the next start (checked with an independent exFAT driver, fragmented files, spaces in names and sub-folders, BIOS and
  UEFI, in an emulator). The app shows them the next time you read the drive.
* **ISOs NyBoot does not know**: it does not need a ready-made recipe for every distro. An image of an unknown distro
  gets a generic start (it looks for a kernel and initrd on the image), and an image with its own `grub.cfg` can
  use that menu. This is why images Ventoy-style tools struggle with, such as CachyOS, are meant to work. Not every ISO
  can boot from a loop device (Windows installers cannot), and NyForge says so when you add one.
* **NyFormat**: choose a drive, a filesystem (only the ones this system can really make), a label, and press **Format...**.
* Closing the window while a write is running asks first, then stops the write.
* The progress and results are the engine's own. Nothing is simulated, and there is no "verified" claim, because the
  engine does not read a write back (see Known limits).
* Animations follow your system's "reduce motion" setting.

## Command line

Everything the app does is also available as `nyforge`, plus a few extras (Windows installer media, Raspberry Pi
first-boot setup, cloning a drive into an image). Every command can print one JSON event per line with `--json`,
which is exactly how the app talks to the engine.


Drives are named the way your system names them: `/dev/sdb` (Linux), `/dev/disk4` (macOS; `/dev/rdisk4` also works),
`\\.\PhysicalDrive2` (Windows; plain `PhysicalDrive2` also works). `nyforge list` shows the right names.
Examples use the Linux names; use `sudo` on Linux and macOS, an Administrator terminal on Windows.

```sh
nyforge list                                   # drives you can write to
nyforge inspect ubuntu.iso                     # what is this image?
sudo nyforge flash ubuntu.iso /dev/sdb         # write it (asks first)
sudo nyforge flash raspios.img.xz /dev/sdb /dev/sdc --hostname mypi --user me --ssh --ssh-key ~/.ssh/id_ed25519.pub
sudo nyforge windows Win11.iso /dev/sdb --bypass-checks
sudo nyforge boot install /dev/sdb             # multiboot drive
sudo nyforge boot add /dev/sdb *.iso
nyforge boot check some.iso
sudo nyforge format                            # pick from the USB drives and SD cards that can be formatted
sudo nyforge format /dev/sdb --fs exfat -l HOLIDAY
sudo nyforge format /dev/sdb --scheme gpt --partition fat32:16G:WIN --partition ext4::LINUX
sudo nyforge format /dev/sdb --test-capacity full --wipe zero      # unmask fake capacity, then erase everything
nyforge format --list                          # what could be formatted (never internal disks)
nyforge clone /dev/sdb backup.img.zst
nyforge doctor                                 # does this machine have what NyForge needs?
```

Every destructive command accepts `-y/--yes`; commands that write to a file path instead of a drive
(`nyforge flash x.iso out.img`) never need root, which is also how the test-suite works.
Exit codes: 0 success, 1 failure, 2 bad usage, 130 cancelled.

## Build and test (developers)

```sh
cargo build --release -p nyforge -p nyforge-gui
cargo test -p nyforge-core                     # unit tests
# QEMU boot tests (need qemu-system-x86_64, OVMF, parted, mtools, dosfstools, grub, wimlib, xorriso):
tools/make-test-kernel.sh /some/dir          # downloads a kernel, builds a tiny initramfs
NYFORGE_TEST_KERNEL_DIR=/some/dir cargo test -p nyforge-core --test nyboot_qemu --test winmedia_qemu -- --ignored --test-threads=1
```

GRUB boot code in `assets/grub` is prebuilt (`tools/build-grub-assets.sh` rebuilds it from distro GRUB).

## What has been tested

NyForge is checked in three different ways, and they are not equally strong. Here is which is which.

**Run and passing on Linux x86-64** (automated tests in the development environment):

- Writing to image files and to loop-style targets, zero-skipping, multi-target fan-out, cancellation
  (including stopping a running write by closing the worker's stdin), decompression formats.
- ISO9660 (Rock Ridge, Joliet) and UDF readers; ISO analysis.
- Splitting an `install.wim` and the resulting FAT32 (checked with `fsck.vfat` and `wimlib-imagex`).
- Booting under QEMU: a NyBoot drive with a real Linux kernel, and a Windows-style stick (BIOS and UEFI),
  using stand-in files for the Windows parts.
- First-boot files against the published cloud-init and Raspberry Pi formats.
- `nyforge format`: FAT16, FAT32, exFAT, NTFS and ext4 volumes in image files, checked with `fsck.vfat`,
  `fsck.exfat`, `e2fsck`, `ntfsfix` and `blkid`; MBR and GPT (both copies, all CRCs) checked with `parted`;
  the capacity test against simulated wrap-around and black-hole fake drives; and, on a Linux loop device,
  the uncached (O_DIRECT) path, zero and random wipes, and ext4/NTFS written by `mkfs` to a real device node.
  The refusal of system, internal, virtual, read-only and wrongly-classified drives is unit-tested, and
  was tried on this machine's own disks as an unprivileged user.
- The desktop app, on Linux (under a virtual display, with real engine runs): flashing an image and comparing it
  byte for byte; NyBoot install, add and remove; NyFormat FAT32 and exFAT (label checked with `blkid`, `fsck.exfat`
  clean); a failed run showing the engine's real error; a drive disappearing between selection and start; closing the
  window while a write runs; keyboard navigation; the Windows "restart as administrator" notice (previewed on Linux
  with a test switch). Stopping a write works at the engine level (both ways the app can ask, exit code 130).
- The portable Linux build (glibc 2.17 baseline, linked with Zig) was built and started on Linux x86-64;
  its highest glibc requirement was checked to be 2.17.

**Written to be portable and unit-tested on Linux, but never run on the real system:** the macOS and Windows
host code. The decisions that matter are plain functions with tests that run everywhere:

- macOS: reading `diskutil` output and deciding what is a safe drive (12 tests against sample output, including
  "the Mac booted from this USB stick", "built-in SD slot", "disk image", "unknown bus"), which disk node to open
  (`/dev/rdiskN`), unmount command, whole-sector I/O.
- Windows: reading the PowerShell drive list (including the quirks of its JSON), drive names, the NTFS format script
  (values reach it only through environment variables).

The parts that call the operating system itself (opening `/dev/rdiskN`, `F_NOCACHE`, `diskutil unmountDisk`,
`DeviceIoControl` locking and dismounting volumes, the Windows elevation check, `Format-Volume`) have **not been
compiled or run** by the author: the development machine could not build for those systems. The repository's CI
workflow builds and tests on real macOS and Windows runners, lists the machine's drives there, and tries
flashing a RAM disk (macOS) and a virtual disk (Windows) and reading it back. Those two steps are marked
"allowed to fail" until someone has seen them pass once. If you run NyForge on those systems before that,
you are the first tester: use a spare stick.

**Not tested at all**, so treat as unproven:

- The desktop app on macOS and Windows: it is written for them (the macOS administrator prompt through `osascript`,
  the Windows UAC restart, the PowerShell drive list) but has never been started on either system by the author.
  CI compiles it there on real runners; nothing in CI clicks through the app.
- The Linux authorization window (pkexec/polkit) and the native file chooser: the test display had neither
  (file selection was driven by a test switch, drag and drop uses the same code path).
- The app writing to a real, physical USB stick on any system.

- A real Windows Setup was never booted (there was no Windows ISO). The UEFI path uses the ISO's own
  loader and is the safer bet; the legacy-BIOS path (`ntldr` chain via GRUB) is less proven.
- The Windows 11 check-bypass answer file on a real Setup.
- Real USB hardware on any system (everything ran on image files and a loop device, never a physical stick),
  and real Raspberry Pi first boot.
- `nyforge format` on a real drive's partitions: the step where the kernel creates `/dev/sdb1` after the new table
  is written, and `mkfs` then runs on it. (The loop driver of the test machine cannot have partitions; the
  naming and waiting code is unit-tested, the kernel part is not.) FAT/exFAT partitions do not need it.
- The Linux ARM64 and static (musl) builds: they are built and started by CI (under an emulator), never on this machine.
- The capacity test on a genuinely counterfeit stick. It is proven against simulated fakes; the quick mode samples
  the drive and can miss a fake whose real size is not a multiple of the sampling distance (the full mode cannot).
  Pi first-boot files follow Raspberry Pi's documentation; newer Raspberry Pi OS releases may handle some
  settings (Wi-Fi, hostname, time zone) differently.
- NyBoot with distros other than Ubuntu-style images; profiles for Debian-live, Arch-family (Arch, EndeavourOS,
  CachyOS, Garuda, SystemRescue: any kernel name, any base folder), Manjaro, Fedora-style, "has its own grub.cfg"
  images and a generic last-resort profile are checked against ISO layouts in an emulator, but not yet on real ISOs
  (the CachyOS one follows CachyOS's own published boot options).

**NyBoot start-up details.** NyBoot starts Arch-family images (Arch, CachyOS, EndeavourOS ...) itself instead of showing
the image's own GRUB menu, because that menu begins with a search of every partition of every disk in the computer,
which can take a long time. To get an image's own menu back, put a file named `<image>.iso.nyboot.cfg` next to it
containing `set nb_profile="loopbackcfg"`. The same kind of file can add start options, for example
`set nb_params="nomodeset"`.

**Known limits**

- NyBoot does not support Secure Boot, persistence, or ISOs that cannot boot from a loop device yet.
- MBR partitioning only; drives up to 2 TiB with 512-byte sectors.
- `install.esd` larger than 4 GiB is not supported (normal Windows ISOs use `install.wim`).
- NyForge does not read the drive back to verify a write (the format capacity test does read back, by design).
- `nyforge format` makes FAT16, FAT32 and exFAT itself on every system. ext4 and NTFS need Linux and the system's
  `mkfs.ext4` / `mkfs.ntfs` (`nyforge doctor` says if they are there); on Windows NTFS is made by Windows itself
  (and needs a partition table, not `--scheme none`); macOS can make neither. 512-byte-sector drives only;
  MBR reaches 2 TB, so bigger drives default to GPT; GPT cannot be combined with `--fit-real-capacity`.
- Windows 10 or newer (the drive list uses PowerShell's Storage module). The macOS download is one universal build;
  its Intel half is only cross-compiled by CI. There is no Windows ARM64 download (build from source if you need it).
- The desktop app does not update NyBoot in place (the button is shown disabled); reinstalling erases the drive.
- macOS and Windows builds are not code-signed or notarized yet.

## Contributing and publishing

Maintainers: [docs/PUBLISHING.md](docs/PUBLISHING.md) explains how to put this repository on GitHub and publish a
release from the Omarchy terminal.

## License

GPL-3.0-or-later. See `LICENSE` and `THIRD-PARTY.md`.
