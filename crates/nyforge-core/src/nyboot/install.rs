//! Turn a drive (or an image file) into a NyBoot drive.

use super::{assets, esp, exfat::{ExFat, FormatParams}, io::{FileIo, Io}, layout::{self, Layout, SECTOR}};
use crate::error::{ErrorKind, NyError, Result};
use crate::format::human_size;
use crate::progress::{Event, Phase};
use crate::sys::{self, TargetKind};
use crate::writer::Target;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const DATA_LABEL: &str = "NYBOOT";

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub label: String,
    pub esp_mib: u64,
    /// Space to leave unpartitioned at the end of the drive.
    pub reserve_mib: u64,
    /// Required when the target is a plain image file: its size.
    pub file_size: Option<u64>,
    /// Extra files for the boot partition (path, bytes). Used by tests.
    pub extra_esp_files: Vec<(String, Vec<u8>)>,
    /// Files to put on the data partition right away.
    pub extra_data_files: Vec<(String, Vec<u8>)>,
    pub unmount: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            label: DATA_LABEL.into(),
            esp_mib: 64,
            reserve_mib: 0,
            file_size: None,
            extra_esp_files: vec![],
            extra_data_files: vec![],
            unmount: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallReport {
    pub layout: Layout,
    pub data_bytes: u64,
}

fn random_u32() -> u32 {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    u32::from_le_bytes(b) | 0x0100_0000 // never zero, never tiny
}

fn check_cancel(c: &AtomicBool) -> Result<()> {
    if c.load(Ordering::Relaxed) {
        Err(NyError::cancelled())
    } else {
        Ok(())
    }
}

pub fn install(target: &Target, opts: &InstallOptions, cancel: Arc<AtomicBool>, emit: &mut dyn FnMut(Event)) -> Result<InstallReport> {
    let disk_bytes = match target.kind {
        TargetKind::BlockDevice => target.capacity.ok_or_else(|| NyError::other("unknown drive size"))?,
        TargetKind::File => opts
            .file_size
            .or(target.capacity)
            .ok_or_else(|| NyError::invalid("Making a NyBoot image file needs a size.").with_hint("Add --size 4G (for example)."))?,
    };
    if let Some(d) = &target.drive {
        if d.sector_size != 512 {
            return Err(NyError::new(
                ErrorKind::ImageUnsupported,
                format!("{} uses {}-byte sectors; NyBoot supports 512-byte sectors only for now.", d.path, d.sector_size),
            ));
        }
    }
    let layout = Layout::plan(disk_bytes, opts.esp_mib, opts.reserve_mib)?;

    emit(Event::Start {
        image: "NyBoot".into(),
        container: "multiboot".into(),
        total_bytes: Some(disk_bytes),
        targets: vec![target.display.clone()],
    });
    emit(Event::Phase { phase: Phase::Preparing, detail: "Releasing the drive".into() });
    let _prep = match &target.drive {
        Some(d) => Some(sys::prepare_device_for_erase(d, opts.unmount)?),
        None => None,
    };

    let file = open_rw_for_install(target).map_err(|e| NyError::from_io(&e, &format!("opening {}", target.display)))?;
    if target.kind == TargetKind::File {
        file.set_len(disk_bytes).map_err(|e| NyError::from_io(&e, "sizing the image file"))?;
    }
    let align = sys::block_io_align(target.kind);
    let whole = FileIo::new(file, 0, layout.disk_sectors * SECTOR, align).recovering(&target.path, target.drive.as_ref().filter(|_| target.kind == TargetKind::BlockDevice), sys::Reopen::Rw);

    emit(Event::Phase { phase: Phase::Writing, detail: "Writing the partition table and boot code".into() });
    // Old tables, boot sectors and the old backup GPT must not survive.
    whole.zero(0, 1 << 20).map_err(|e| NyError::from_io(&e, "clearing the start of the drive"))?;
    whole
        .zero(whole.len() - (1 << 20), 1 << 20)
        .map_err(|e| NyError::from_io(&e, "clearing the end of the drive"))?;
    check_cancel(&cancel)?;
    let mbr = layout::build_mbr(&layout, random_u32());
    whole.write_at(&assets::CORE_IMG.to_vec().into_padded(), SECTOR).map_err(|e| NyError::from_io(&e, "writing GRUB"))?;
    whole.write_at(&mbr, 0).map_err(|e| NyError::from_io(&e, "writing the boot sector"))?;

    emit(Event::Phase { phase: Phase::Writing, detail: format!("Formatting the {} data partition", human_size(layout.data_bytes())) });
    let data_io = whole.sub(layout.data_start * SECTOR, layout.data_bytes()).map_err(|e| NyError::from_io(&e, "partitioning"))?;
    let mut fs = ExFat::format(data_io, &FormatParams { label: opts.label.clone(), serial: random_u32(), cluster_bytes: None })?;
    for (path, bytes) in &opts.extra_data_files {
        fs.add_file(path, &mut &bytes[..], bytes.len() as u64, &mut |_| true)?;
    }
    fs.sync()?;
    check_cancel(&cancel)?;

    emit(Event::Phase { phase: Phase::Writing, detail: "Writing the UEFI boot partition".into() });
    let esp_bytes = layout.esp_sectors * SECTOR;
    let image = esp::build_esp_image(esp_bytes, random_u32(), &opts.extra_esp_files)?;
    let esp_io = whole.sub(layout.esp_start * SECTOR, esp_bytes).map_err(|e| NyError::from_io(&e, "partitioning"))?;
    // Write in 4 MiB pieces so progress and cancel stay responsive on a slow stick.
    let step = 4usize << 20;
    for (i, chunk) in image.chunks(step).enumerate() {
        esp_io.write_at(chunk, (i * step) as u64).map_err(|e| NyError::from_io(&e, "writing the UEFI boot partition"))?;
        check_cancel(&cancel)?;
    }

    emit(Event::Phase { phase: Phase::Finalizing, detail: "Flushing to the drive".into() });
    whole.sync().map_err(|e| NyError::from_io(&e, "flushing the drive"))?;
    drop(whole);
    if target.kind == TargetKind::BlockDevice {
        sys::finish_device(&target.path);
    }
    emit(Event::TargetFinished { target: target.display.clone(), ok: true, bytes: disk_bytes, error: None });
    emit(Event::Finished { ok: true, bytes: disk_bytes, elapsed_secs: 0.0, avg_speed_bps: 0.0 });
    Ok(InstallReport { layout, data_bytes: layout.data_bytes() })
}

/// Read+write handle: drives exclusively (the kernel refuses if anything still uses them),
/// image files created fresh.
pub(crate) fn open_rw_for_install(target: &Target) -> std::io::Result<std::fs::File> {
    match target.kind {
        TargetKind::BlockDevice => sys::open_rw(&target.path, true),
        TargetKind::File => std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&target.path),
    }
}

trait IntoPadded {
    fn into_padded(self) -> Vec<u8>;
}

impl IntoPadded for Vec<u8> {
    fn into_padded(mut self) -> Vec<u8> {
        self.resize(self.len().next_multiple_of(SECTOR as usize), 0);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nyboot::layout::parse_mbr;
    use std::process::Command;

    /// Removes a test's scratch directory when the test ends, pass or fail (these hold gigabytes).
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nyforge-install-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn installs_into_an_image_file() {
        let d = tdir("img");
        let _cleanup = Cleanup(d.clone());
        let img = d.join("nyboot.img");
        let size = 1u64 << 30;
        let opts = InstallOptions {
            file_size: Some(size),
            extra_data_files: vec![("README.txt".into(), b"hello nyboot".to_vec())],
            ..Default::default()
        };
        let mut events = vec![];
        let rep = install(&Target::file(&img), &opts, Arc::new(AtomicBool::new(false)), &mut |e| events.push(e)).unwrap();
        assert_eq!(std::fs::metadata(&img).unwrap().len(), size);
        assert!(matches!(events.last().unwrap(), Event::Finished { ok: true, .. }));

        let raw = std::fs::read(&img).unwrap();
        let parts = parse_mbr(&raw[..512]).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], (0x07, rep.layout.data_start, rep.layout.data_sectors));
        assert_eq!(parts[1], (0xEF, rep.layout.esp_start, rep.layout.esp_sectors));
        // GRUB's core.img sits at LBA 1, byte for byte.
        assert_eq!(&raw[512..512 + assets::CORE_IMG.len()], assets::CORE_IMG);
        assert_eq!(raw[0x66..0x68], [0x90, 0x90]);

        // Reopen the data partition with our own reader, and with fsck.exfat.
        let f = std::fs::OpenOptions::new().read(true).write(true).open(&img).unwrap();
        let io = FileIo::new(f, rep.layout.data_start * SECTOR, rep.layout.data_bytes(), 1);
        let fs = ExFat::open(io).unwrap();
        assert_eq!(fs.label().unwrap(), "NYBOOT");
        let e = fs.stat("README.txt").unwrap().unwrap();
        let mut b = vec![0u8; e.size as usize];
        fs.read_file(&e, 0, &mut b).unwrap();
        assert_eq!(b, b"hello nyboot");

        let data = d.join("data.img");
        let mut out = std::fs::File::create(&data).unwrap();
        use std::io::{Read, Seek, SeekFrom, Write};
        let mut src = std::fs::File::open(&img).unwrap();
        src.seek(SeekFrom::Start(rep.layout.data_start * SECTOR)).unwrap();
        let mut left = rep.layout.data_bytes();
        let mut buf = vec![0u8; 1 << 20];
        while left > 0 {
            let n = left.min(buf.len() as u64) as usize;
            src.read_exact(&mut buf[..n]).unwrap();
            out.write_all(&buf[..n]).unwrap();
            left -= n as u64;
        }
        drop(out);
        if let Ok(o) = Command::new("fsck.exfat").arg(&data).output() {
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
        }
    }

    #[test]
    fn refuses_too_small_and_missing_size() {
        let d = tdir("small");
        let _cleanup = Cleanup(d.clone());
        let e = install(&Target::file(d.join("a.img")), &InstallOptions::default(), Arc::new(AtomicBool::new(false)), &mut |_| {}).err().unwrap();
        assert_eq!(e.kind, ErrorKind::InvalidInput);
        let o = InstallOptions { file_size: Some(100 << 20), ..Default::default() };
        let e = install(&Target::file(d.join("b.img")), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).err().unwrap();
        assert_eq!(e.kind, ErrorKind::InvalidInput);
    }
}
