//! Everything NyBoot installs that is not computed: GRUB images and menu scripts.
//! Regenerate the images with `tools/build-grub-assets.sh`.

pub const BOOT_IMG: &[u8] = include_bytes!("../../../../assets/grub/boot.img");
pub const CORE_IMG: &[u8] = include_bytes!("../../../../assets/grub/core.img");
pub const CORE_WIN_IMG: &[u8] = include_bytes!("../../../../assets/grub/core-win.img");
pub const BOOTX64_EFI: &[u8] = include_bytes!("../../../../assets/grub/BOOTX64.EFI");
pub const BOOTIA32_EFI: &[u8] = include_bytes!("../../../../assets/grub/BOOTIA32.EFI");
pub const UNICODE_FONT: &[u8] = include_bytes!("../../../../assets/grub/unicode.pf2");
/// UEFI:NTFS (https://github.com/pbatard/uefi-ntfs): a 1 MiB FAT image with Secure Boot signed UEFI boot loaders and NTFS drivers.
/// It lets a UEFI computer start from a drive whose big partition is NTFS. Copied unchanged from the Rufus project's repository
/// (res/uefi/uefi-ntfs.img); see assets/uefi-ntfs/README-from-rufus.txt and THIRD-PARTY.md.
pub const UEFI_NTFS_IMG: &[u8] = include_bytes!("../../../../assets/uefi-ntfs/uefi-ntfs.img");
pub const GRUB_CFG: &str = include_str!("../../../../assets/grub/cfg/grub.cfg");
pub const PROFILES_CFG: &str = include_str!("../../../../assets/grub/cfg/profiles.cfg");

/// The graphical menu theme, installed as /grub/themes/nyboot/ on the boot partition.
/// Regenerate with `python3 tools/make-theme.py` (image pieces, fonts); theme.txt is written by hand.
// THEME_FILES-BEGIN (written by tools/make-theme.py)
pub const THEME_FILES: &[(&str, &[u8])] = &[
    ("font-bold-24.pf2", include_bytes!("../../../../assets/grub/theme/font-bold-24.pf2")),
    ("font-bold-30.pf2", include_bytes!("../../../../assets/grub/theme/font-bold-30.pf2")),
    ("font-bold-42.pf2", include_bytes!("../../../../assets/grub/theme/font-bold-42.pf2")),
    ("font-regular-15.pf2", include_bytes!("../../../../assets/grub/theme/font-regular-15.pf2")),
    ("font-regular-17.pf2", include_bytes!("../../../../assets/grub/theme/font-regular-17.pf2")),
    ("font-regular-19.pf2", include_bytes!("../../../../assets/grub/theme/font-regular-19.pf2")),
    ("font-regular-21.pf2", include_bytes!("../../../../assets/grub/theme/font-regular-21.pf2")),
    ("font-regular-24.pf2", include_bytes!("../../../../assets/grub/theme/font-regular-24.pf2")),
    ("font-regular-26.pf2", include_bytes!("../../../../assets/grub/theme/font-regular-26.pf2")),
    ("font-regular-30.pf2", include_bytes!("../../../../assets/grub/theme/font-regular-30.pf2")),
    ("font-regular-42.pf2", include_bytes!("../../../../assets/grub/theme/font-regular-42.pf2")),
    ("menu_c.png", include_bytes!("../../../../assets/grub/theme/menu_c.png")),
    ("menu_e.png", include_bytes!("../../../../assets/grub/theme/menu_e.png")),
    ("menu_large_c.png", include_bytes!("../../../../assets/grub/theme/menu_large_c.png")),
    ("menu_large_e.png", include_bytes!("../../../../assets/grub/theme/menu_large_e.png")),
    ("menu_large_n.png", include_bytes!("../../../../assets/grub/theme/menu_large_n.png")),
    ("menu_large_ne.png", include_bytes!("../../../../assets/grub/theme/menu_large_ne.png")),
    ("menu_large_nw.png", include_bytes!("../../../../assets/grub/theme/menu_large_nw.png")),
    ("menu_large_s.png", include_bytes!("../../../../assets/grub/theme/menu_large_s.png")),
    ("menu_large_se.png", include_bytes!("../../../../assets/grub/theme/menu_large_se.png")),
    ("menu_large_sw.png", include_bytes!("../../../../assets/grub/theme/menu_large_sw.png")),
    ("menu_large_w.png", include_bytes!("../../../../assets/grub/theme/menu_large_w.png")),
    ("menu_n.png", include_bytes!("../../../../assets/grub/theme/menu_n.png")),
    ("menu_ne.png", include_bytes!("../../../../assets/grub/theme/menu_ne.png")),
    ("menu_nw.png", include_bytes!("../../../../assets/grub/theme/menu_nw.png")),
    ("menu_s.png", include_bytes!("../../../../assets/grub/theme/menu_s.png")),
    ("menu_se.png", include_bytes!("../../../../assets/grub/theme/menu_se.png")),
    ("menu_sw.png", include_bytes!("../../../../assets/grub/theme/menu_sw.png")),
    ("menu_w.png", include_bytes!("../../../../assets/grub/theme/menu_w.png")),
    ("menu_xlarge_c.png", include_bytes!("../../../../assets/grub/theme/menu_xlarge_c.png")),
    ("menu_xlarge_e.png", include_bytes!("../../../../assets/grub/theme/menu_xlarge_e.png")),
    ("menu_xlarge_n.png", include_bytes!("../../../../assets/grub/theme/menu_xlarge_n.png")),
    ("menu_xlarge_ne.png", include_bytes!("../../../../assets/grub/theme/menu_xlarge_ne.png")),
    ("menu_xlarge_nw.png", include_bytes!("../../../../assets/grub/theme/menu_xlarge_nw.png")),
    ("menu_xlarge_s.png", include_bytes!("../../../../assets/grub/theme/menu_xlarge_s.png")),
    ("menu_xlarge_se.png", include_bytes!("../../../../assets/grub/theme/menu_xlarge_se.png")),
    ("menu_xlarge_sw.png", include_bytes!("../../../../assets/grub/theme/menu_xlarge_sw.png")),
    ("menu_xlarge_w.png", include_bytes!("../../../../assets/grub/theme/menu_xlarge_w.png")),
    ("scrollbar_frame_c.png", include_bytes!("../../../../assets/grub/theme/scrollbar_frame_c.png")),
    ("scrollbar_frame_large_c.png", include_bytes!("../../../../assets/grub/theme/scrollbar_frame_large_c.png")),
    ("scrollbar_frame_large_n.png", include_bytes!("../../../../assets/grub/theme/scrollbar_frame_large_n.png")),
    ("scrollbar_frame_large_s.png", include_bytes!("../../../../assets/grub/theme/scrollbar_frame_large_s.png")),
    ("scrollbar_frame_n.png", include_bytes!("../../../../assets/grub/theme/scrollbar_frame_n.png")),
    ("scrollbar_frame_s.png", include_bytes!("../../../../assets/grub/theme/scrollbar_frame_s.png")),
    ("scrollbar_frame_xlarge_c.png", include_bytes!("../../../../assets/grub/theme/scrollbar_frame_xlarge_c.png")),
    ("scrollbar_frame_xlarge_n.png", include_bytes!("../../../../assets/grub/theme/scrollbar_frame_xlarge_n.png")),
    ("scrollbar_frame_xlarge_s.png", include_bytes!("../../../../assets/grub/theme/scrollbar_frame_xlarge_s.png")),
    ("scrollbar_thumb_c.png", include_bytes!("../../../../assets/grub/theme/scrollbar_thumb_c.png")),
    ("scrollbar_thumb_large_c.png", include_bytes!("../../../../assets/grub/theme/scrollbar_thumb_large_c.png")),
    ("scrollbar_thumb_large_n.png", include_bytes!("../../../../assets/grub/theme/scrollbar_thumb_large_n.png")),
    ("scrollbar_thumb_large_s.png", include_bytes!("../../../../assets/grub/theme/scrollbar_thumb_large_s.png")),
    ("scrollbar_thumb_n.png", include_bytes!("../../../../assets/grub/theme/scrollbar_thumb_n.png")),
    ("scrollbar_thumb_s.png", include_bytes!("../../../../assets/grub/theme/scrollbar_thumb_s.png")),
    ("scrollbar_thumb_xlarge_c.png", include_bytes!("../../../../assets/grub/theme/scrollbar_thumb_xlarge_c.png")),
    ("scrollbar_thumb_xlarge_n.png", include_bytes!("../../../../assets/grub/theme/scrollbar_thumb_xlarge_n.png")),
    ("scrollbar_thumb_xlarge_s.png", include_bytes!("../../../../assets/grub/theme/scrollbar_thumb_xlarge_s.png")),
    ("select_c.png", include_bytes!("../../../../assets/grub/theme/select_c.png")),
    ("select_e.png", include_bytes!("../../../../assets/grub/theme/select_e.png")),
    ("select_large_c.png", include_bytes!("../../../../assets/grub/theme/select_large_c.png")),
    ("select_large_e.png", include_bytes!("../../../../assets/grub/theme/select_large_e.png")),
    ("select_large_w.png", include_bytes!("../../../../assets/grub/theme/select_large_w.png")),
    ("select_w.png", include_bytes!("../../../../assets/grub/theme/select_w.png")),
    ("select_xlarge_c.png", include_bytes!("../../../../assets/grub/theme/select_xlarge_c.png")),
    ("select_xlarge_e.png", include_bytes!("../../../../assets/grub/theme/select_xlarge_e.png")),
    ("select_xlarge_w.png", include_bytes!("../../../../assets/grub/theme/select_xlarge_w.png")),
    ("terminal_box_c.png", include_bytes!("../../../../assets/grub/theme/terminal_box_c.png")),
    ("terminal_box_e.png", include_bytes!("../../../../assets/grub/theme/terminal_box_e.png")),
    ("terminal_box_large_c.png", include_bytes!("../../../../assets/grub/theme/terminal_box_large_c.png")),
    ("terminal_box_large_e.png", include_bytes!("../../../../assets/grub/theme/terminal_box_large_e.png")),
    ("terminal_box_large_n.png", include_bytes!("../../../../assets/grub/theme/terminal_box_large_n.png")),
    ("terminal_box_large_ne.png", include_bytes!("../../../../assets/grub/theme/terminal_box_large_ne.png")),
    ("terminal_box_large_nw.png", include_bytes!("../../../../assets/grub/theme/terminal_box_large_nw.png")),
    ("terminal_box_large_s.png", include_bytes!("../../../../assets/grub/theme/terminal_box_large_s.png")),
    ("terminal_box_large_se.png", include_bytes!("../../../../assets/grub/theme/terminal_box_large_se.png")),
    ("terminal_box_large_sw.png", include_bytes!("../../../../assets/grub/theme/terminal_box_large_sw.png")),
    ("terminal_box_large_w.png", include_bytes!("../../../../assets/grub/theme/terminal_box_large_w.png")),
    ("terminal_box_n.png", include_bytes!("../../../../assets/grub/theme/terminal_box_n.png")),
    ("terminal_box_ne.png", include_bytes!("../../../../assets/grub/theme/terminal_box_ne.png")),
    ("terminal_box_nw.png", include_bytes!("../../../../assets/grub/theme/terminal_box_nw.png")),
    ("terminal_box_s.png", include_bytes!("../../../../assets/grub/theme/terminal_box_s.png")),
    ("terminal_box_se.png", include_bytes!("../../../../assets/grub/theme/terminal_box_se.png")),
    ("terminal_box_sw.png", include_bytes!("../../../../assets/grub/theme/terminal_box_sw.png")),
    ("terminal_box_w.png", include_bytes!("../../../../assets/grub/theme/terminal_box_w.png")),
    ("terminal_box_xlarge_c.png", include_bytes!("../../../../assets/grub/theme/terminal_box_xlarge_c.png")),
    ("terminal_box_xlarge_e.png", include_bytes!("../../../../assets/grub/theme/terminal_box_xlarge_e.png")),
    ("terminal_box_xlarge_n.png", include_bytes!("../../../../assets/grub/theme/terminal_box_xlarge_n.png")),
    ("terminal_box_xlarge_ne.png", include_bytes!("../../../../assets/grub/theme/terminal_box_xlarge_ne.png")),
    ("terminal_box_xlarge_nw.png", include_bytes!("../../../../assets/grub/theme/terminal_box_xlarge_nw.png")),
    ("terminal_box_xlarge_s.png", include_bytes!("../../../../assets/grub/theme/terminal_box_xlarge_s.png")),
    ("terminal_box_xlarge_se.png", include_bytes!("../../../../assets/grub/theme/terminal_box_xlarge_se.png")),
    ("terminal_box_xlarge_sw.png", include_bytes!("../../../../assets/grub/theme/terminal_box_xlarge_sw.png")),
    ("terminal_box_xlarge_w.png", include_bytes!("../../../../assets/grub/theme/terminal_box_xlarge_w.png")),
    ("theme-large.txt", include_bytes!("../../../../assets/grub/theme/theme-large.txt")),
    ("theme-xlarge.txt", include_bytes!("../../../../assets/grub/theme/theme-xlarge.txt")),
    ("theme.txt", include_bytes!("../../../../assets/grub/theme/theme.txt")),
];
// THEME_FILES-END

/// Memtest86+ v7.00 (GPL-2, unmodified; see assets/grub/memtest/README.txt). Installed as /grub/memtest/ and started by the
/// "Memtest86+" menu entry: .bin through GRUB's `linux` command on BIOS, .efi on UEFI (x64 and 32-bit).
pub const MEMTEST_FILES: &[(&str, &[u8])] = &[
    ("memtest86+x64.bin", include_bytes!("../../../../assets/grub/memtest/memtest86+x64.bin")),
    ("memtest86+ia32.bin", include_bytes!("../../../../assets/grub/memtest/memtest86+ia32.bin")),
    ("memtest86+x64.efi", include_bytes!("../../../../assets/grub/memtest/memtest86+x64.efi")),
    ("memtest86+ia32.efi", include_bytes!("../../../../assets/grub/memtest/memtest86+ia32.efi")),
    ("LICENSE-GPL-2.txt", include_bytes!("../../../../assets/grub/memtest/LICENSE-GPL-2.txt")),
    ("README.txt", include_bytes!("../../../../assets/grub/memtest/README.txt")),
];

/// Written to the boot partition so `nyboot update` knows what is installed.
pub const VERSION: &str = concat!("nyboot ", env!("CARGO_PKG_VERSION"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grub_images_look_right() {
        assert_eq!(BOOT_IMG.len(), 512);
        // core.img must fit in the gap before partition 1 (LBA 1..2047).
        assert!(CORE_IMG.len() <= 2047 * 512, "core.img is {} bytes", CORE_IMG.len());
        assert!(CORE_WIN_IMG.len() <= 2047 * 512);
        assert_eq!(&BOOTX64_EFI[..2], b"MZ");
        assert_eq!(&BOOTIA32_EFI[..2], b"MZ");
        assert_eq!(UEFI_NTFS_IMG.len(), 1 << 20);
        assert_eq!(&UEFI_NTFS_IMG[0x36..0x3E], b"FAT12   ");
        assert_eq!(&UEFI_NTFS_IMG[510..512], &[0x55, 0xAA]);
    }

    #[test]
    fn every_theme_size_has_the_pieces_and_fonts_it_names() {
        let has = |n: &str| THEME_FILES.iter().any(|(f, _)| *f == n);
        for size in ["", "-large", "-xlarge"] {
            let name = format!("theme{size}.txt");
            let txt = THEME_FILES.iter().find(|(f, _)| *f == name).map(|(_, d)| std::str::from_utf8(d).unwrap()).unwrap_or_else(|| panic!("{name} missing"));
            for line in txt.lines().filter(|l| !l.trim_start().starts_with('#')) {
                let Some(q) = line.split('"').nth(1) else { continue };
                if let Some(base) = q.strip_suffix("_*.png") {
                    let parts: &[&str] = if base.starts_with("select") { &["c", "w", "e"] } else if base.starts_with("scrollbar") { &["c", "n", "s"] } else { &["c", "n", "s", "e", "w", "ne", "nw", "se", "sw"] };
                    for p in parts {
                        assert!(has(&format!("{base}_{p}.png")), "{name}: {base}_{p}.png missing");
                    }
                } else if let Some(font) = q.strip_prefix("Liberation Sans ") {
                    // "Plex Regular 18" -> plex-regular-18.pf2
                    let mut it = font.split(' ');
                    let (style, px) = (it.next().unwrap().to_lowercase(), it.next().unwrap());
                    assert!(has(&format!("font-{style}-{px}.pf2")), "{name}: font {q} missing");
                }
            }
        }
        // It all has to fit on the smallest boot partition (32 MiB).
        let total: usize = THEME_FILES.iter().map(|(_, d)| d.len()).sum();
        assert!(total < 2 << 20, "theme is {total} bytes");
    }
}
