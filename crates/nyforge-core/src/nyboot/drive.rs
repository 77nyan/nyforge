//! Working with a drive that already is a NyBoot drive: list, add and remove ISOs.

use super::{exfat::{Entry, ExFat}, io::{FileIo, Io}, layout::{self, SECTOR}};
use crate::error::{ErrorKind, NyError, Result};
use crate::format::human_size;
use crate::image::ImageSource;
use crate::progress::*;
use crate::sys::{self, DevicePrep, TargetKind};
use crate::writer::Target;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct OpenDrive {
    pub fs: ExFat<FileIo>,
    pub data_bytes: u64,
    _prep: Option<DevicePrep>,
}

fn not_nyboot(why: &str) -> NyError {
    NyError::new(ErrorKind::InvalidInput, format!("This is not a NyBoot drive ({why})."))
        .with_hint("Make one with: nyforge boot install <drive>")
}

/// Open the data partition of a NyBoot drive or image file.
pub fn open(target: &Target, unmount: bool) -> Result<OpenDrive> {
    let prep = match &target.drive {
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
    let align = sys::block_io_align(target.kind);
    let whole = FileIo::new(file, 0, total, align);
    let mut mbr = [0u8; 512];
    whole.read_at(&mut mbr, 0).map_err(|e| NyError::from_io(&e, "reading the partition table"))?;
    let parts = layout::parse_mbr(&mbr).ok_or_else(|| not_nyboot("no partition table"))?;
    let data = parts.iter().find(|p| p.0 == 0x07).ok_or_else(|| not_nyboot("no data partition"))?;
    if !parts.iter().any(|p| p.0 == 0xEF) {
        return Err(not_nyboot("no boot partition"));
    }
    let data_io = whole.sub(data.1 * SECTOR, data.2 * SECTOR).map_err(|e| NyError::from_io(&e, "reading the partition table"))?;
    let fs = ExFat::open(data_io).map_err(|_| not_nyboot("the data partition is not exFAT"))?;
    Ok(OpenDrive { fs, data_bytes: data.2 * SECTOR, _prep: prep })
}

/// Strip a compression extension from a name: `x.iso.xz` → `x.iso`.
pub fn plain_name(name: &str) -> String {
    for ext in [".xz", ".gz", ".bz2", ".zst", ".zip"] {
        if let Some(stem) = name.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    name.to_string()
}

/// Copy an image onto the drive (decompressing on the fly).
pub fn add_image(
    drive: &mut OpenDrive,
    source: ImageSource,
    dest_dir: &str,
    cancel: Arc<AtomicBool>,
    emit: &mut dyn FnMut(Event),
) -> Result<Entry> {
    let size = source.info.decompressed_size.ok_or_else(|| {
        NyError::new(ErrorKind::ImageUnsupported, "The size of that file is not known until it is unpacked.")
            .with_hint("Unpack it first (gzip and bzip2 don't record their size), then add the .iso.")
    })?;
    let name = plain_name(source.info.inner_name.as_deref().unwrap_or_else(|| {
        let l = source.info.label.as_str();
        l.rsplit(['/', '\\']).next().unwrap_or(l)
    }));
    let dest = format!("{}/{}", dest_dir.trim_matches('/'), name);
    let dest = dest.trim_start_matches('/').to_string();
    if size > drive.fs.free_bytes() {
        return Err(NyError::new(
            ErrorKind::NoSpace,
            format!("{} does not fit: {} free on the drive, {} needed.", name, human_size(drive.fs.free_bytes()), human_size(size)),
        ));
    }
    emit(Event::Start {
        image: name.clone(),
        container: source.info.container.name().to_string(),
        total_bytes: Some(size),
        targets: vec![format!("/{dest}")],
    });
    emit(Event::Phase { phase: Phase::Writing, detail: format!("Copying {name}") });
    let started = Instant::now();
    let mut meter = SpeedMeter::new(Duration::from_secs(3));
    let mut last = Instant::now() - Duration::from_secs(1);
    let target_name = format!("/{dest}");
    let mut reader = source.reader;
    let entry = drive.fs.add_file(&dest, &mut reader, size, &mut |done| {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        if last.elapsed() >= Duration::from_millis(120) {
            last = Instant::now();
            let mut speed = meter.sample(Instant::now(), done);
            if speed <= 0.0 {
                speed = done as f64 / started.elapsed().as_secs_f64().max(0.001);
            }
            emit(Event::Progress(Progress {
                bytes_done: done,
                bytes_total: Some(size),
                fraction: (done as f64 / size.max(1) as f64).clamp(0.0, 1.0),
                speed_bps: speed,
                eta_secs: if speed > 1.0 { Some((size - done) as f64 / speed) } else { None },
                targets: vec![TargetProgress { target: target_name.clone(), bytes: done, state: TargetState::Writing }],
            }));
        }
        true
    })?;
    drive.fs.sync()?;
    let el = started.elapsed().as_secs_f64();
    emit(Event::TargetFinished { target: target_name, ok: true, bytes: size, error: None });
    emit(Event::Finished { ok: true, bytes: size, elapsed_secs: el, avg_speed_bps: if el > 0.0 { size as f64 / el } else { 0.0 } });
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nyboot::install::{install, InstallOptions};

    #[test]
    fn add_list_remove_on_an_installed_image() {
        let d = std::env::temp_dir().join(format!("nyforge-drive-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let img = d.join("nb.img");
        let opts = InstallOptions { file_size: Some(768 << 20), esp_mib: 32, ..Default::default() };
        install(&Target::file(&img), &opts, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();

        let iso = d.join("fake.iso");
        let data: Vec<u8> = (0..5_000_000u32).map(|i| (i % 253) as u8).collect();
        std::fs::write(&iso, &data).unwrap();
        // Also a compressed one: stored under its plain name.
        use std::io::Write;
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 1);
        enc.write_all(&data).unwrap();
        let xz = d.join("packed.iso.xz");
        std::fs::write(&xz, enc.finish().unwrap()).unwrap();

        let mut od = open(&Target::file(&img), true).unwrap();
        add_image(&mut od, crate::image::open_file(&iso).unwrap(), "iso", Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
        add_image(&mut od, crate::image::open_file(&xz).unwrap(), "", Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
        let names: Vec<String> = od.fs.list("/").unwrap().into_iter().map(|e| e.name).collect();
        assert!(names.contains(&"packed.iso".to_string()), "{names:?}");
        let e = od.fs.stat("iso/fake.iso").unwrap().unwrap();
        assert_eq!(e.size, data.len() as u64);
        let mut back = vec![0u8; data.len()];
        od.fs.read_file(&e, 0, &mut back).unwrap();
        assert_eq!(back, data);
        od.fs.remove("iso/fake.iso").unwrap();
        assert!(od.fs.stat("iso/fake.iso").unwrap().is_none());
        drop(od);

        // A plain file is not a NyBoot drive.
        let junk = d.join("junk.img");
        std::fs::write(&junk, vec![0u8; 4096]).unwrap();
        assert!(open(&Target::file(&junk), true).is_err());
    }

    #[test]
    fn plain_names() {
        assert_eq!(plain_name("a.iso.xz"), "a.iso");
        assert_eq!(plain_name("a.iso"), "a.iso");
        assert_eq!(plain_name("a.iso.zip"), "a.iso");
    }
}
