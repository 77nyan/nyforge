//! Everything NyBoot installs that is not computed: GRUB images and menu scripts.
//! Regenerate the images with `tools/build-grub-assets.sh`.

pub const BOOT_IMG: &[u8] = include_bytes!("../../../../assets/grub/boot.img");
pub const CORE_IMG: &[u8] = include_bytes!("../../../../assets/grub/core.img");
pub const CORE_WIN_IMG: &[u8] = include_bytes!("../../../../assets/grub/core-win.img");
pub const BOOTX64_EFI: &[u8] = include_bytes!("../../../../assets/grub/BOOTX64.EFI");
pub const BOOTIA32_EFI: &[u8] = include_bytes!("../../../../assets/grub/BOOTIA32.EFI");
pub const UNICODE_FONT: &[u8] = include_bytes!("../../../../assets/grub/unicode.pf2");
pub const GRUB_CFG: &str = include_str!("../../../../assets/grub/cfg/grub.cfg");
pub const PROFILES_CFG: &str = include_str!("../../../../assets/grub/cfg/profiles.cfg");

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
    }
}
