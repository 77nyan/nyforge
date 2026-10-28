//! The small FAT32 boot partition: UEFI GRUB, the menu scripts and the font.
//! It is built completely in memory and written in one go.

use super::assets;
use crate::error::{NyError, Result};
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use std::io::{Cursor, Write};

pub const ESP_LABEL: &str = "NYBOOT-EFI";

fn fat_err(e: std::io::Error) -> NyError {
    NyError::other(format!("Building the boot partition failed: {e}"))
}

/// A ready-to-write image of the boot partition (`size` bytes).
pub fn build_esp_image(size: u64, volume_id: u32, extra: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut label = [b' '; 11];
    label[..ESP_LABEL.len()].copy_from_slice(ESP_LABEL.as_bytes());
    let mut disk = Cursor::new(vec![0u8; size as usize]);
    fatfs::format_volume(
        &mut disk,
        FormatVolumeOptions::new().fat_type(FatType::Fat32).volume_label(label).volume_id(volume_id).fats(2),
    )
    .map_err(fat_err)?;
    {
        let fs = FileSystem::new(&mut disk, FsOptions::new()).map_err(fat_err)?;
        {
        let root = fs.root_dir();
        let put = |dir: &fatfs::Dir<'_, &mut Cursor<Vec<u8>>>, name: &str, data: &[u8]| -> Result<()> {
            let mut f = dir.create_file(name).map_err(fat_err)?;
            f.write_all(data).map_err(fat_err)?;
            f.flush().map_err(fat_err)
        };
        let efi = root.create_dir("EFI").map_err(fat_err)?;
        let boot = efi.create_dir("BOOT").map_err(fat_err)?;
        put(&boot, "BOOTX64.EFI", assets::BOOTX64_EFI)?;
        put(&boot, "BOOTIA32.EFI", assets::BOOTIA32_EFI)?;
        let grub = root.create_dir("grub").map_err(fat_err)?;
        put(&grub, "grub.cfg", assets::GRUB_CFG.as_bytes())?;
        put(&grub, "profiles.cfg", assets::PROFILES_CFG.as_bytes())?;
        put(&grub, "unicode.pf2", assets::UNICODE_FONT)?;
        // The menu theme lives in grub/themes/nyboot/ (icons/ is a sub-folder).
        let themes = grub.create_dir("themes").map_err(fat_err)?;
        let theme = themes.create_dir("nyboot").map_err(fat_err)?;
        let icons = theme.create_dir("icons").map_err(fat_err)?;
        for (name, data) in assets::THEME_FILES {
            match name.strip_prefix("icons/") {
                Some(icon) => put(&icons, icon, data)?,
                None => put(&theme, name, data)?,
            }
        }
        let memtest = grub.create_dir("memtest").map_err(fat_err)?;
        for (name, data) in assets::MEMTEST_FILES {
            put(&memtest, name, data)?;
        }
        put(&grub, "nyboot.version", format!("{}\n", assets::VERSION).as_bytes())?;
        for (path, data) in extra {
            let mut dir = root.clone();
            let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
            let (name, dirs) = parts.split_last().ok_or_else(|| NyError::invalid("empty extra file name"))?;
            for d in dirs {
                dir = dir.create_dir(d).map_err(fat_err)?;
            }
            put(&dir, name, data)?;
        }
        }
        let _ = fs.stats(); // fills in FSInfo's free-cluster count
        fs.unmount().map_err(fat_err)?;
    }
    Ok(disk.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn esp_image_has_the_expected_files() {
        let img = build_esp_image(64 << 20, 0x1234_5678, &[("grub/test.cfg".into(), b"set x=1\n".to_vec())]).unwrap();
        assert_eq!(img.len(), 64 << 20);
        let fs = FileSystem::new(Cursor::new(img), FsOptions::new()).unwrap();
        assert_eq!(fs.volume_label().trim(), ESP_LABEL);
        assert_eq!(fs.fat_type(), FatType::Fat32);
        let root = fs.root_dir();
        let mut s = String::new();
        root.open_file("grub/test.cfg").unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "set x=1\n");
        let mut head = [0u8; 2];
        root.open_file("EFI/BOOT/BOOTX64.EFI").unwrap().read_exact(&mut head).unwrap();
        assert_eq!(&head, b"MZ");
        let mut cfg = String::new();
        root.open_file("grub/grub.cfg").unwrap().read_to_string(&mut cfg).unwrap();
        assert!(cfg.contains("nb_scan"));
        for f in ["grub/memtest/memtest86+x64.bin", "grub/memtest/memtest86+x64.efi", "grub/memtest/memtest86+ia32.efi", "grub/memtest/memtest86+ia32.bin", "grub/themes/nyboot/theme.txt", "grub/themes/nyboot/theme-large.txt"] {
            assert!(root.open_file(f).is_ok(), "{f} missing from the boot partition");
        }
        assert!(cfg.contains("Memtest86+") && cfg.contains("Shutdown") && !cfg.contains("\"Restart\""));
    }

    #[test]
    fn esp_image_passes_fsck() {
        let img = build_esp_image(64 << 20, 0x1234_5678, &[("grub/test.cfg".into(), b"set x=1\n".to_vec())]).unwrap();
        let p = std::env::temp_dir().join(format!("nyforge-esp-fsck-{}.img", std::process::id()));
        std::fs::write(&p, img).unwrap();
        if let Ok(o) = std::process::Command::new("fsck.vfat").arg("-n").arg(&p).output() {
            assert!(o.status.success(), "fsck.vfat: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        }
    }
}
