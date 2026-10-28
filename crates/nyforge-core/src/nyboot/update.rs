//! Update the boot code and menu of an existing NyBoot drive without touching the ISOs on it.
//!
//! What is replaced: the GRUB boot code in the first sector, `core.img` in the gap before the first partition,
//! and the whole small UEFI partition (EFI loaders, menu scripts, font). What is kept: the disk signature and the
//! partition table (so nothing moves), the data partition with every ISO, and the user's own `nyboot.cfg`.

use super::{assets, esp, io::{FileIo, Io}, layout::{self, SECTOR}};
use crate::error::{ErrorKind, NyError, Result};
use crate::progress::{Event, Phase};
use crate::sys::{self, TargetKind};
use crate::writer::Target;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct UpdateReport {
    /// What the drive had (`None` if it carried no version marker).
    pub from: Option<String>,
    pub to: String,
    pub esp_bytes: u64,
}

/// Read-only view of a region for `fatfs`, which wants `Read + Write + Seek`; writing is refused.
struct ReadOnly<'a> {
    io: &'a dyn Io,
    pos: u64,
}

impl Read for ReadOnly<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.io.len().saturating_sub(self.pos);
        let n = (buf.len() as u64).min(left) as usize;
        if n == 0 {
            return Ok(0);
        }
        self.io.read_at(&mut buf[..n], self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for ReadOnly<'_> {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "read-only"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for ReadOnly<'_> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let p = match to {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.io.len() as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if p < 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = p as u64;
        Ok(self.pos)
    }
}

/// A file of the boot partition, or `None` if the partition is not a FAT volume that has it.
fn read_esp_file(esp: &dyn Io, path: &str, max: usize) -> Option<Vec<u8>> {
    let fs = fatfs::FileSystem::new(ReadOnly { io: esp, pos: 0 }, fatfs::FsOptions::new()).ok()?;
    let mut f = fs.root_dir().open_file(path).ok()?;
    let mut v = Vec::new();
    Read::take(&mut f, max as u64).read_to_end(&mut v).ok()?;
    Some(v)
}

fn not_nyboot(why: &str) -> NyError {
    NyError::new(ErrorKind::InvalidInput, format!("This is not a NyBoot drive ({why}), so it cannot be updated."))
        .with_hint("Make one with: nyforge boot install <drive>")
}

/// The exact layout a NyBoot drive has: partition 1 exFAT data (type 7) starting at 1 MiB, partition 2 the FAT32
/// boot partition (type 0xEF) after it, and nothing else. Anything different is not ours to rewrite.
fn find_layout(mbr: &[u8]) -> Result<(u64, u64, u64, u64)> {
    let parts = layout::parse_mbr(mbr).ok_or_else(|| not_nyboot("no partition table"))?;
    let data = parts.iter().find(|p| p.0 == 0x07).ok_or_else(|| not_nyboot("no data partition"))?;
    let esp = parts.iter().find(|p| p.0 == 0xEF).ok_or_else(|| not_nyboot("no boot partition"))?;
    if parts.len() != 2 || esp.1 < data.1 + data.2 || data.1 < (assets::CORE_IMG.len() as u64).div_ceil(SECTOR) + 1 {
        return Err(not_nyboot("the partitions are not laid out the way NyBoot makes them"));
    }
    Ok((data.1, data.2, esp.1, esp.2))
}

/// The version text on the drive's boot partition, if it is a NyBoot drive.
pub fn installed_version(target: &Target) -> Result<Option<String>> {
    let file = match target.kind {
        TargetKind::BlockDevice => sys::open_read(&target.path, target.kind, false),
        TargetKind::File => std::fs::File::open(&target.path),
    }
    .map_err(|e| NyError::from_io(&e, &format!("opening {}", target.display)))?;
    let total = match target.kind {
        TargetKind::BlockDevice => target.capacity.unwrap_or(0),
        TargetKind::File => file.metadata().map_err(|e| NyError::from_io(&e, "reading the file size"))?.len(),
    };
    let whole = FileIo::new(file, 0, total, sys::block_io_align(target.kind));
    let mut mbr = [0u8; 512];
    whole.read_at(&mut mbr, 0).map_err(|e| NyError::from_io(&e, "reading the partition table"))?;
    let (_, _, es, el) = find_layout(&mbr)?;
    let esp = whole.sub(es * SECTOR, el * SECTOR).map_err(|e| NyError::from_io(&e, "reading the boot partition"))?;
    Ok(read_esp_file(&esp, "grub/nyboot.version", 256).map(|v| String::from_utf8_lossy(&v).trim().to_string()))
}

pub fn update(target: &Target, unmount: bool, cancel: Arc<AtomicBool>, emit: &mut dyn FnMut(Event)) -> Result<UpdateReport> {
    let check = |c: &AtomicBool| if c.load(Ordering::Relaxed) { Err(NyError::cancelled()) } else { Ok(()) };
    if let Some(d) = &target.drive {
        if d.sector_size != 512 {
            return Err(NyError::new(ErrorKind::ImageUnsupported, format!("{} uses {}-byte sectors; NyBoot supports 512-byte sectors only for now.", d.path, d.sector_size)));
        }
    }
    emit(Event::Start { image: "NyBoot update".into(), container: "multiboot".into(), total_bytes: None, targets: vec![target.display.clone()] });
    emit(Event::Phase { phase: Phase::Preparing, detail: "Checking the drive".into() });
    let _prep = match &target.drive {
        Some(d) => Some(sys::prepare_device(d, unmount)?),
        None => None,
    };
    let file = match target.kind {
        TargetKind::BlockDevice => sys::open_rw(&target.path, true),
        TargetKind::File => std::fs::OpenOptions::new().read(true).write(true).open(&target.path),
    }
    .map_err(|e| NyError::from_io(&e, &format!("opening {}", target.display)))?;
    let total = match target.kind {
        TargetKind::BlockDevice => target.capacity.unwrap_or(0),
        TargetKind::File => file.metadata().map_err(|e| NyError::from_io(&e, "reading the file size"))?.len(),
    };
    let whole = FileIo::new(file, 0, total, sys::block_io_align(target.kind))
        .recovering(&target.path, target.drive.as_ref().filter(|_| target.kind == TargetKind::BlockDevice), sys::Reopen::Rw);
    let mut mbr = [0u8; 512];
    whole.read_at(&mut mbr, 0).map_err(|e| NyError::from_io(&e, "reading the partition table"))?;
    let (data_start, data_len, esp_start, esp_len) = find_layout(&mbr)?;
    let esp_io = whole.sub(esp_start * SECTOR, esp_len * SECTOR).map_err(|e| NyError::from_io(&e, "reading the boot partition"))?;
    // Only a boot partition that NyBoot itself made is replaced: the version marker is the proof.
    let marker = read_esp_file(&esp_io, "grub/nyboot.version", 256).ok_or_else(|| not_nyboot("its boot partition has no NyBoot marker"))?;
    let from = Some(String::from_utf8_lossy(&marker).trim().to_string()).filter(|s| !s.is_empty());
    // The test hook, only present on drives built by NyForge's own tests, survives an update.
    let keep: Vec<(String, Vec<u8>)> = read_esp_file(&esp_io, "grub/test.cfg", 1 << 20).map(|b| vec![("grub/test.cfg".to_string(), b)]).unwrap_or_default();
    let serial = {
        let mut s = [0u8; 4];
        esp_io.read_at(&mut s, 0x43).map_err(|e| NyError::from_io(&e, "reading the boot partition"))?;
        u32::from_le_bytes(s)
    };
    check(&cancel)?;

    emit(Event::Phase { phase: Phase::Writing, detail: "Writing the new UEFI boot partition".into() });
    let esp_bytes = esp_len * SECTOR;
    let image = esp::build_esp_image(esp_bytes, serial, &keep)?;
    let step = 4usize << 20;
    for (i, chunk) in image.chunks(step).enumerate() {
        esp_io.write_at(chunk, (i * step) as u64).map_err(|e| NyError::from_io(&e, "writing the UEFI boot partition"))?;
        check(&cancel)?;
    }
    drop(image);

    // Boot code last: from here on the drive starts with the new GRUB, and the partition it reads is complete.
    emit(Event::Phase { phase: Phase::Writing, detail: "Writing the new boot code".into() });
    let fresh = layout::build_mbr(
        &layout::Layout { disk_sectors: total / SECTOR, data_start, data_sectors: data_len, esp_start, esp_sectors: esp_len, reserved_sectors: 0 },
        0,
    );
    // Keep the old disk signature and partition table byte for byte; replace only the boot code (bytes 0..440).
    let mut merged = mbr;
    merged[..440].copy_from_slice(&fresh[..440]);
    let mut core = assets::CORE_IMG.to_vec();
    core.resize(core.len().div_ceil(SECTOR as usize) * SECTOR as usize, 0);
    whole.write_at(&core, SECTOR).map_err(|e| NyError::from_io(&e, "writing GRUB"))?;
    whole.write_at(&merged, 0).map_err(|e| NyError::from_io(&e, "writing the boot sector"))?;
    emit(Event::Phase { phase: Phase::Finalizing, detail: "Flushing to the drive".into() });
    whole.sync().map_err(|e| NyError::from_io(&e, "flushing the drive"))?;
    drop(whole);
    if target.kind == TargetKind::BlockDevice {
        sys::finish_device(&target.path);
    }
    emit(Event::TargetFinished { target: target.display.clone(), ok: true, bytes: 0, error: None });
    emit(Event::Finished { ok: true, bytes: 0, elapsed_secs: 0.0, avg_speed_bps: 0.0 });
    Ok(UpdateReport { from, to: assets::VERSION.to_string(), esp_bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nyboot::{config, drive, install::{install, InstallOptions}};

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nyforge-upd-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn update_replaces_boot_code_and_menu_but_keeps_everything_else() {
        let d = scratch("keep");
        let img = d.join("nb.img");
        let opts = InstallOptions { file_size: Some(768 << 20), esp_mib: 32, ..Default::default() };
        install(&Target::file(&img), &opts, flag(), &mut |_| {}).unwrap();
        let iso = d.join("x.iso");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&iso, &data).unwrap();
        {
            let mut od = drive::open(&Target::file(&img), true).unwrap();
            drive::add_image(&mut od, crate::image::open_file(&iso).unwrap(), "iso", flag(), &mut |_| {}).unwrap();
            config::save(&mut od, &config::MenuConfig { timeout_secs: Some(7), ..Default::default() }).unwrap();
        }
        // Make the drive look like an older NyBoot: damage the boot code, GRUB and the boot partition's menu.
        let before = std::fs::read(&img).unwrap();
        let (ds, dl, es, el) = find_layout(&before[..512]).unwrap();
        {
            use std::os::unix::fs::FileExt;
            let f = std::fs::OpenOptions::new().write(true).open(&img).unwrap();
            f.write_all_at(&[0xAA; 440], 0).unwrap();
            f.write_all_at(&vec![0xBB; 4096], 512).unwrap();
            f.write_all_at(&vec![0u8; 4096], es * SECTOR).unwrap(); // the boot partition's first sectors
        }
        assert!(installed_version(&Target::file(&img)).is_err() || installed_version(&Target::file(&img)).unwrap().is_none());
        // With the boot sector gone it must refuse rather than guess.
        let r = update(&Target::file(&img), true, flag(), &mut |_| {});
        assert!(r.is_err(), "a damaged boot partition has no marker and must not be rewritten");
        // Put the damaged ESP back from before and try again: only the boot code is old now.
        {
            use std::os::unix::fs::FileExt;
            let f = std::fs::OpenOptions::new().write(true).open(&img).unwrap();
            f.write_all_at(&before[(es * SECTOR) as usize..(es * SECTOR) as usize + 4096], es * SECTOR).unwrap();
        }
        let rep = update(&Target::file(&img), true, flag(), &mut |_| {}).unwrap();
        assert_eq!(rep.to, assets::VERSION);
        assert_eq!(rep.from.as_deref(), Some(assets::VERSION));
        let after = std::fs::read(&img).unwrap();
        // Boot code and GRUB are back; the disk signature and the partition table never moved.
        assert_eq!(&after[..440], &before[..440]);
        assert_eq!(&after[440..512], &before[440..512]);
        assert_eq!(&after[512..512 + assets::CORE_IMG.len()], &assets::CORE_IMG[..]);
        // The data partition is byte for byte what it was, and the ISO and settings are still readable.
        let (a, b) = ((ds * SECTOR) as usize, ((ds + dl) * SECTOR) as usize);
        assert!(after[a..b] == before[a..b], "the data partition changed");
        let od = drive::open(&Target::file(&img), true).unwrap();
        let e = od.fs.stat("iso/x.iso").unwrap().unwrap();
        let mut back = vec![0u8; data.len()];
        od.fs.read_file(&e, 0, &mut back).unwrap();
        assert_eq!(back, data);
        assert_eq!(config::load(&od).unwrap().timeout_secs, Some(7));
        let _ = (es, el);
        assert_eq!(installed_version(&Target::file(&img)).unwrap().as_deref(), Some(assets::VERSION));
    }

    #[test]
    fn a_stick_that_is_not_nyboot_is_never_touched() {
        let d = scratch("not");
        let img = d.join("plain.img");
        let mut mbr = vec![0u8; 8 << 20];
        // An ordinary FAT32-looking MBR: one partition, type 0x0C.
        mbr[446 + 4] = 0x0C;
        mbr[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
        mbr[446 + 12..446 + 16].copy_from_slice(&8000u32.to_le_bytes());
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        std::fs::write(&img, &mbr).unwrap();
        assert!(update(&Target::file(&img), true, flag(), &mut |_| {}).is_err());
        assert_eq!(std::fs::read(&img).unwrap(), mbr, "a refused update must not write anything");
        // A drive that has the NyBoot partition types but an ESP NyBoot did not make is refused too.
        let mut m2 = mbr.clone();
        m2[446 + 4] = 0x07;
        m2[462 + 4] = 0xEF;
        m2[462 + 8..462 + 12].copy_from_slice(&6000u32.to_le_bytes());
        m2[462 + 12..462 + 16].copy_from_slice(&2000u32.to_le_bytes());
        std::fs::write(&img, &m2).unwrap();
        assert!(update(&Target::file(&img), true, flag(), &mut |_| {}).is_err());
        assert_eq!(std::fs::read(&img).unwrap(), m2);
    }
}
