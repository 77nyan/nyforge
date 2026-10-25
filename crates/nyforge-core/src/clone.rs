//! Drive → image file (backup / clone), optionally compressed on the fly.

use crate::error::{ErrorKind, NyError, Result};
use crate::format::human_size;
use crate::image;
use crate::progress::*;
use crate::writer::{write_image, Target, WriteOptions, WriteReport};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Zstd,
    Xz,
}

impl Compression {
    /// Pick from the output file name: `.gz`, `.zst`, `.xz`; anything else is raw.
    pub fn from_path(p: &Path) -> Self {
        match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
            Some("gz") => Compression::Gzip,
            Some("zst") | Some("zstd") => Compression::Zstd,
            Some("xz") => Compression::Xz,
            _ => Compression::None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Compression::None => "raw",
            Compression::Gzip => "gzip",
            Compression::Zstd => "zstd",
            Compression::Xz => "xz",
        }
    }
}

enum Sink {
    Gz(flate2::write::GzEncoder<File>),
    Zst(zstd::stream::write::Encoder<'static, File>),
    Xz(xz2::write::XzEncoder<File>),
}

impl Sink {
    fn write_all(&mut self, b: &[u8]) -> std::io::Result<()> {
        match self {
            Sink::Gz(w) => w.write_all(b),
            Sink::Zst(w) => w.write_all(b),
            Sink::Xz(w) => w.write_all(b),
        }
    }
    fn finish(self) -> std::io::Result<()> {
        match self {
            Sink::Gz(w) => w.finish().and_then(|f| f.sync_all()),
            Sink::Zst(w) => w.finish().and_then(|f| f.sync_all()),
            Sink::Xz(w) => w.finish().and_then(|f| f.sync_all()),
        }
    }
}

/// Read `size` bytes from `source` (a drive or file) into `out`.
pub fn clone_to_file(
    source: &Path,
    size: u64,
    label: &str,
    out: &Path,
    comp: Compression,
    cancel: Arc<AtomicBool>,
    emit: &mut dyn FnMut(Event),
) -> Result<WriteReport> {
    if comp == Compression::None {
        let src = image::open_device_source(source, size)?;
        return write_image(src, vec![Target::file(out)], &WriteOptions::default(), cancel, emit);
    }

    let src = image::open_device_source(source, size)?;
    let mut reader = src.reader;
    let fout = File::create(out).map_err(|e| NyError::from_io(&e, &format!("creating {}", out.display())))?;
    let mut sink = match comp {
        Compression::Gzip => Sink::Gz(flate2::write::GzEncoder::new(fout, flate2::Compression::new(6))),
        Compression::Zstd => {
            let mut enc = zstd::stream::write::Encoder::new(fout, 6).map_err(|e| NyError::from_io(&e, "starting compression"))?;
            let _ = enc.multithread(std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1));
            let _ = enc.include_contentsize(false);
            Sink::Zst(enc)
        }
        Compression::Xz => Sink::Xz(xz2::write::XzEncoder::new(fout, 6)),
        Compression::None => unreachable!(),
    };

    emit(Event::Start { image: label.to_string(), container: comp.name().to_string(), total_bytes: Some(size), targets: vec![out.display().to_string()] });
    emit(Event::Phase { phase: Phase::Writing, detail: format!("Compressing with {}", comp.name()) });

    let started = Instant::now();
    let mut meter = SpeedMeter::new(Duration::from_secs(3));
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0u64;
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let target_name = out.display().to_string();
    let fail = |emit: &mut dyn FnMut(Event), e: NyError| -> NyError {
        let _ = std::fs::remove_file(out);
        emit(Event::Error { error: e.clone() });
        e
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(fail(emit, NyError::cancelled()));
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(fail(emit, NyError::from_io(&e, &format!("reading {label}")))),
        };
        if let Err(e) = sink.write_all(&buf[..n]) {
            return Err(fail(emit, NyError::from_io(&e, &format!("writing {}", out.display()))));
        }
        done += n as u64;
        if last_emit.elapsed() >= Duration::from_millis(120) {
            last_emit = Instant::now();
            let speed = meter.sample(Instant::now(), done);
            emit(Event::Progress(Progress {
                bytes_done: done,
                bytes_total: Some(size),
                fraction: if size > 0 { (done as f64 / size as f64).clamp(0.0, 1.0) } else { 0.0 },
                speed_bps: speed,
                eta_secs: if speed > 1.0 { Some(size.saturating_sub(done) as f64 / speed) } else { None },
                targets: vec![TargetProgress { target: target_name.clone(), bytes: done, state: TargetState::Writing }],
            }));
        }
    }
    if done < size {
        return Err(fail(
            emit,
            NyError::new(ErrorKind::Hardware, format!("The drive stopped answering after {} of {}.", human_size(done), human_size(size)))
                .with_hint("The drive may be failing or was unplugged."),
        ));
    }
    emit(Event::Phase { phase: Phase::Finalizing, detail: "Finishing the file".into() });
    if let Err(e) = sink.finish() {
        return Err(fail(emit, NyError::from_io(&e, &format!("finishing {}", out.display()))));
    }
    let elapsed = started.elapsed();
    emit(Event::TargetFinished { target: target_name.clone(), ok: true, bytes: done, error: None });
    emit(Event::Finished {
        ok: true,
        bytes: done,
        elapsed_secs: elapsed.as_secs_f64(),
        avg_speed_bps: if elapsed.as_secs_f64() > 0.0 { done as f64 / elapsed.as_secs_f64() } else { 0.0 },
    });
    Ok(WriteReport {
        image_bytes: done,
        elapsed,
        targets: vec![crate::writer::TargetReport { target: target_name, ok: true, bytes: done, error: None }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 13 + (i >> 9)) % 247) as u8).collect()
    }

    #[test]
    fn picks_compression_from_extension() {
        assert_eq!(Compression::from_path(Path::new("a.img.zst")), Compression::Zstd);
        assert_eq!(Compression::from_path(Path::new("a.IMG.GZ")), Compression::Gzip);
        assert_eq!(Compression::from_path(Path::new("a.xz")), Compression::Xz);
        assert_eq!(Compression::from_path(Path::new("a.img")), Compression::None);
    }

    #[test]
    fn clone_roundtrips_for_every_format() {
        let dir = std::env::temp_dir().join(format!("nyforge-clone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("disk.img");
        let bytes = data(5 * 1024 * 1024 + 77);
        std::fs::write(&src, &bytes).unwrap();
        for name in ["out.img", "out.img.gz", "out.img.zst", "out.img.xz"] {
            let out = dir.join(name);
            let comp = Compression::from_path(&out);
            let r = clone_to_file(&src, bytes.len() as u64, "test disk", &out, comp, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
            assert!(r.all_ok(), "{name}");
            // Read it back through the normal image path: proves the output is something we can flash again.
            let mut s = image::open_file(&out).unwrap();
            let mut got = Vec::new();
            s.reader.read_to_end(&mut got).unwrap();
            assert_eq!(got, bytes, "{name}");
        }
    }

    #[test]
    fn short_source_is_reported_as_hardware_trouble() {
        let dir = std::env::temp_dir().join(format!("nyforge-clone-short-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("disk.img");
        std::fs::write(&src, data(1000)).unwrap();
        let out = dir.join("o.img.gz");
        let e = clone_to_file(&src, 1_000_000, "x", &out, Compression::Gzip, Arc::new(AtomicBool::new(false)), &mut |_| {}).err().unwrap();
        assert_eq!(e.kind, ErrorKind::Hardware);
        assert!(!out.exists(), "partial output must be removed");
    }
}
