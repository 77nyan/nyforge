//! Image sources: anything that can become a stream of raw bytes to write.
//!
//! Formats are recognised by their magic bytes, never by file extension, so a
//! mis-named download still works.  Compressed images are decompressed on the
//! fly; nothing is ever extracted to disk first.

use crate::error::{ErrorKind, IoContext, NyError, Result};
use serde::Serialize;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom, Take};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    Raw,
    Gzip,
    Xz,
    Bzip2,
    Zstd,
    Zip,
}

impl Container {
    pub fn name(self) -> &'static str {
        match self {
            Container::Raw => "raw",
            Container::Gzip => "gzip",
            Container::Xz => "xz",
            Container::Bzip2 => "bzip2",
            Container::Zstd => "zstd",
            Container::Zip => "zip",
        }
    }
}

/// Describes an image without reading all of it.
#[derive(Debug, Clone, Serialize)]
pub struct ImageInfo {
    pub label: String,
    pub container: Container,
    /// Bytes on disk / over the wire.
    pub compressed_size: Option<u64>,
    /// Size once decompressed, when we can know it for certain.
    pub decompressed_size: Option<u64>,
    /// Name of the file inside an archive, if any.
    pub inner_name: Option<String>,
}

pub struct ImageSource {
    pub reader: Box<dyn Read + Send>,
    pub info: ImageInfo,
    /// Compressed bytes consumed so far (for progress when the output size is unknown).
    pub consumed: Arc<AtomicU64>,
}

impl ImageSource {
    pub fn total_hint(&self) -> Option<u64> {
        self.info.decompressed_size
    }
}

struct CountingRead<R> {
    inner: R,
    n: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.n.fetch_add(k as u64, Ordering::Relaxed);
        Ok(k)
    }
}

pub fn sniff_container(head: &[u8]) -> Container {
    if head.starts_with(&[0x1f, 0x8b]) {
        Container::Gzip
    } else if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        Container::Xz
    } else if head.starts_with(b"BZh") && head.len() > 3 && (b'1'..=b'9').contains(&head[3]) {
        Container::Bzip2
    } else if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Container::Zstd
    } else if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        Container::Zip
    } else {
        Container::Raw
    }
}

/// Formats we recognise but cannot write as-is. Better to say so than to
/// write garbage onto someone's drive.
fn reject_unwritable(head: &[u8]) -> Result<()> {
    let msg = if head.starts_with(b"QFI\xfb") {
        Some("a QCOW2 virtual disk")
    } else if head.starts_with(b"vhdxfile") {
        Some("a VHDX virtual disk")
    } else if head.starts_with(b"<<< Oracle VM VirtualBox Disk Image >>>") {
        Some("a VirtualBox VDI disk")
    } else if head.starts_with(b"KDMV") {
        Some("a VMware VMDK disk")
    } else if head.starts_with(b"7z\xbc\xaf\x27\x1c") {
        Some("a 7-Zip archive")
    } else if head.starts_with(b"Rar!\x1a\x07") {
        Some("a RAR archive")
    } else {
        None
    };
    match msg {
        Some(what) => Err(NyError::new(
            ErrorKind::ImageUnsupported,
            format!("This file is {what}, not a disk image NyForge can write directly."),
        )
        .with_hint("Convert or extract it to a raw .img/.iso first.")),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------- size probes

fn read_vli(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v: u64 = 0;
    for i in 0..9 {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= ((b & 0x7f) as u64) << (7 * i);
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Exact decompressed size of a single-stream .xz file, read from its index.
pub fn xz_uncompressed_size<R: Read + Seek>(f: &mut R, len: u64) -> Option<u64> {
    if len < 32 {
        return None;
    }
    f.seek(SeekFrom::Start(len - 12)).ok()?;
    let mut footer = [0u8; 12];
    f.read_exact(&mut footer).ok()?;
    if &footer[10..12] != b"YZ" {
        return None;
    }
    let backward = u32::from_le_bytes(footer[4..8].try_into().ok()?) as u64;
    let index_size = (backward + 1) * 4;
    if index_size + 24 > len {
        return None;
    }
    f.seek(SeekFrom::Start(len - 12 - index_size)).ok()?;
    let mut index = vec![0u8; index_size as usize];
    f.read_exact(&mut index).ok()?;
    if index[0] != 0 {
        return None;
    }
    let mut pos = 1;
    let records = read_vli(&index, &mut pos)?;
    let (mut total, mut blocks_bytes) = (0u64, 0u64);
    for _ in 0..records {
        let unpadded = read_vli(&index, &mut pos)?;
        let uncompressed = read_vli(&index, &mut pos)?;
        total = total.checked_add(uncompressed)?;
        blocks_bytes += unpadded.div_ceil(4) * 4;
    }
    // header(12) + blocks + index + footer(12) must be the whole file, else it
    // has several concatenated streams or padding and we cannot be sure.
    if 12 + blocks_bytes + index_size + 12 == len {
        Some(total)
    } else {
        None
    }
}

fn zstd_content_size<R: Read + Seek>(f: &mut R) -> Option<u64> {
    let mut head = [0u8; 32];
    f.seek(SeekFrom::Start(0)).ok()?;
    let n = f.read(&mut head).ok()?;
    zstd::zstd_safe::get_frame_content_size(&head[..n]).ok().flatten()
}

// --------------------------------------------------------------------- opening

type BoxRead = Box<dyn Read + Send>;

/// Runs a decoder on its own thread, a few blocks ahead of the consumer, so decompressing and everything the reader
/// does with the data (filling chunks, the zero check, handing them to the writers) overlap on a multi-core machine.
/// On one core it costs one extra thread switch per block. Dropping it stops the thread at its next block.
struct PrefetchReader {
    rx: crossbeam_channel::Receiver<std::io::Result<Vec<u8>>>,
    cur: Vec<u8>,
    pos: usize,
    done: bool,
    failed: Option<std::io::Error>,
}

const PREFETCH_BLOCK: usize = 1 << 20;
const PREFETCH_DEPTH: usize = 4;

impl PrefetchReader {
    fn new(mut inner: BoxRead) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(PREFETCH_DEPTH);
        let spawned = std::thread::Builder::new().name("nyforge-decode".into()).spawn(move || loop {
            let mut buf = vec![0u8; PREFETCH_BLOCK];
            let mut n = 0;
            while n < buf.len() {
                match inner.read(&mut buf[n..]) {
                    Ok(0) => break,
                    Ok(k) => n += k,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                }
            }
            buf.truncate(n);
            let last = n < PREFETCH_BLOCK;
            if n > 0 && tx.send(Ok(buf)).is_err() {
                return;
            }
            if last {
                return;
            }
        });
        // A thread that cannot start is an error on the first read, never a silently short image.
        PrefetchReader { rx, cur: Vec::new(), pos: 0, done: false, failed: spawned.err() }
    }
}

impl Read for PrefetchReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if let Some(e) = self.failed.take() {
            self.done = true;
            return Err(e);
        }
        while self.pos >= self.cur.len() {
            if self.done {
                return Ok(0);
            }
            match self.rx.recv() {
                Ok(Ok(b)) => {
                    self.cur = b;
                    self.pos = 0;
                }
                Ok(Err(e)) => {
                    self.done = true;
                    return Err(e);
                }
                Err(_) => {
                    self.done = true;
                    return Ok(0);
                }
            }
        }
        let n = out.len().min(self.cur.len() - self.pos);
        out[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn decoder(container: Container, raw: BoxRead) -> Result<BoxRead> {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let plain: BoxRead = match container {
        Container::Raw => return Ok(raw),
        Container::Gzip => Box::new(flate2::read::MultiGzDecoder::new(BufReader::with_capacity(1 << 20, raw))),
        Container::Xz => Box::new(xz2::read::XzDecoder::new_multi_decoder(BufReader::with_capacity(1 << 20, raw))),
        Container::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(BufReader::with_capacity(1 << 20, raw))),
        Container::Zstd => Box::new(
            zstd::stream::read::Decoder::new(raw)
                .map_err(|e| NyError::from_io(&e, "starting the zstd decoder"))?,
        ),
        Container::Zip => unreachable!("zip handled separately"),
    };
    // A second core is what makes decoding ahead pay off; on a single core it would only add copies.
    Ok(if cores > 1 { Box::new(PrefetchReader::new(plain)) } else { plain })
}

fn is_image_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [".img", ".iso", ".raw", ".bin", ".sdcard", ".rpi-sdimg", ".hddimg", ".dsk", ".etch"]
        .iter()
        .any(|e| n.ends_with(e))
}

fn open_zip(path: &Path, counter: Arc<AtomicU64>, label: String, clen: u64) -> Result<ImageSource> {
    let f = crate::sys::open_sequential(path).ctx("opening the zip file")?;
    let mut ar = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| {
        NyError::new(ErrorKind::ImageCorrupt, format!("This zip file is damaged: {e}"))
            .with_hint("Download it again.")
    })?;
    let mut best: Option<(usize, bool, u64, String)> = None;
    for i in 0..ar.len() {
        let e = ar.by_index_raw(i).map_err(|e| NyError::other(format!("Reading zip entry: {e}")))?;
        if e.is_dir() {
            continue;
        }
        let cand = (i, is_image_name(e.name()), e.size(), e.name().to_string());
        let better = match &best {
            None => true,
            Some(b) => (cand.1, cand.2) > (b.1, b.2),
        };
        if better {
            best = Some(cand);
        }
    }
    let (idx, _, size, name) = best.ok_or_else(|| NyError::new(ErrorKind::ImageUnsupported, "The zip file is empty."))?;
    let (start, csize, method, encrypted) = {
        let e = ar.by_index_raw(idx).map_err(|e| NyError::other(format!("Reading zip entry: {e}")))?;
        (e.data_start(), e.compressed_size(), e.compression(), e.encrypted())
    };
    if encrypted {
        return Err(NyError::new(ErrorKind::ImageUnsupported, "This zip file is password protected.")
            .with_hint("Extract it first, then flash the extracted image."));
    }
    let mut f = crate::sys::open_sequential(path).ctx("reopening the zip file")?;
    f.seek(SeekFrom::Start(start)).ctx("seeking inside the zip file")?;
    let body: Take<CountingRead<File>> = CountingRead { inner: f, n: counter.clone() }.take(csize);
    let reader: BoxRead = match method {
        zip::CompressionMethod::Stored => Box::new(body),
        zip::CompressionMethod::Deflated => Box::new(flate2::read::DeflateDecoder::new(BufReader::new(body))),
        other => {
            return Err(NyError::new(
                ErrorKind::ImageUnsupported,
                format!("This zip uses the {other:?} method, which NyForge does not read."),
            )
            .with_hint("Extract it first, or repack with deflate (normal zip) / .xz."))
        }
    };
    Ok(ImageSource {
        reader,
        info: ImageInfo {
            label,
            container: Container::Zip,
            compressed_size: Some(clen),
            decompressed_size: Some(size),
            inner_name: Some(name),
        },
        consumed: counter,
    })
}

/// Cheap description of an image file (reads only headers/trailers).
pub fn describe_file(path: &Path) -> Result<ImageInfo> {
    let s = open_file(path)?;
    Ok(s.info)
}

pub fn open_file(path: &Path) -> Result<ImageSource> {
    let label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let mut f = crate::sys::open_sequential(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            NyError::new(ErrorKind::InvalidInput, format!("Image not found: {}", path.display()))
        } else {
            NyError::from_io(&e, &format!("opening {}", path.display()))
        }
    })?;
    let meta = f.metadata().ctx("reading the image file's size")?;
    if meta.is_dir() {
        return Err(NyError::invalid(format!("{} is a folder, not an image.", path.display())));
    }
    let clen = meta.len();
    let mut head = [0u8; 64];
    let n = f.read(&mut head).ctx("reading the image header")?;
    reject_unwritable(&head[..n])?;
    let container = sniff_container(&head[..n]);
    let counter = Arc::new(AtomicU64::new(0));

    if container == Container::Zip {
        return open_zip(path, counter, label, clen);
    }

    let decompressed = match container {
        Container::Raw => Some(clen),
        Container::Xz => xz_uncompressed_size(&mut f, clen),
        Container::Zstd => zstd_content_size(&mut f),
        // gzip stores size mod 2^32 and bzip2 stores nothing: unknown for sure.
        _ => None,
    };
    f.seek(SeekFrom::Start(0)).ctx("rewinding the image")?;
    let raw: BoxRead = Box::new(CountingRead { inner: f, n: counter.clone() });
    let reader = decoder(container, raw)?;
    Ok(ImageSource {
        reader,
        info: ImageInfo {
            label,
            container,
            compressed_size: Some(clen),
            decompressed_size: decompressed,
            inner_name: None,
        },
        consumed: counter,
    })
}

pub fn is_url(spec: &str) -> bool {
    let s = spec.trim().to_ascii_lowercase();
    s.starts_with("http://") || s.starts_with("https://")
}

pub fn open_url(url: &str) -> Result<ImageSource> {
    let resp = ureq::get(url).call().map_err(|e| {
        NyError::new(ErrorKind::Network, format!("Could not download {url}: {e}"))
            .with_hint("Check the address and your connection, or download the file first and flash it from disk.")
    })?;
    let clen = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let counter = Arc::new(AtomicU64::new(0));
    let body: BoxRead = Box::new(CountingRead { inner: resp.into_body().into_reader(), n: counter.clone() });

    // Peek the first bytes to learn the container, then stitch them back on.
    let mut body = body;
    let mut head = vec![0u8; 8];
    let mut got = 0;
    while got < head.len() {
        let k = body.read(&mut head[got..]).map_err(|e| NyError::new(ErrorKind::Network, format!("Download failed: {e}")))?;
        if k == 0 {
            break;
        }
        got += k;
    }
    head.truncate(got);
    reject_unwritable(&head)?;
    let container = sniff_container(&head);
    if container == Container::Zip {
        return Err(NyError::new(
            ErrorKind::ImageUnsupported,
            "Zip files cannot be flashed straight from a URL (they need random access).",
        )
        .with_hint("Download it first, or use a .xz / .gz / .zst version of the image."));
    }
    let chained: BoxRead = Box::new(Cursor::new(head).chain(body));
    let reader = decoder(container, chained)?;
    let label = url.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(url).to_string();
    let decompressed = if container == Container::Raw { clen } else { None };
    Ok(ImageSource {
        reader,
        info: ImageInfo { label, container, compressed_size: clen, decompressed_size: decompressed, inner_name: None },
        consumed: counter,
    })
}

/// Open either a file path or an http(s) URL.
pub fn open(spec: &str) -> Result<ImageSource> {
    if is_url(spec) {
        open_url(spec)
    } else {
        open_file(Path::new(spec))
    }
}

/// Read a whole block device (or file) as a source: used for cloning/backup.
pub fn open_device_source(path: &Path, size: u64) -> Result<ImageSource> {
    let f = File::open(path).map_err(|e| NyError::from_io(&e, &format!("opening {} for reading", path.display())))?;
    let counter = Arc::new(AtomicU64::new(0));
    let reader: BoxRead = Box::new(CountingRead { inner: f.take(size), n: counter.clone() });
    Ok(ImageSource {
        reader,
        info: ImageInfo {
            label: path.display().to_string(),
            container: Container::Raw,
            compressed_size: Some(size),
            decompressed_size: Some(size),
            inner_name: None,
        },
        consumed: counter,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nyforge-test-{}-{}", std::process::id(), name));
        d
    }

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 31 + (i >> 8) * 7) % 251) as u8).collect()
    }

    fn read_all(mut s: ImageSource) -> Vec<u8> {
        let mut v = Vec::new();
        s.reader.read_to_end(&mut v).unwrap();
        v
    }

    fn make_zip(path: &std::path::Path, method: zip::CompressionMethod, files: &[(&str, &[u8])]) {
        let f = File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default().compression_method(method);
        for (name, data) in files {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
    }

    #[test]
    fn zip_picks_the_image_and_streams_it() {
        let data = sample(700_000);
        for (tag, method) in [("d", zip::CompressionMethod::Deflated), ("s", zip::CompressionMethod::Stored)] {
            let p = tmp(&format!("pick-{tag}.zip"));
            make_zip(&p, method, &[("README.txt", b"read me, it is longer than nothing"), ("os/disk.img", &data)]);
            let src = open_file(&p).unwrap();
            assert_eq!(src.info.container, Container::Zip);
            assert_eq!(src.info.decompressed_size, Some(data.len() as u64));
            assert_eq!(read_all(src), data, "method {tag}");
            std::fs::remove_file(p).ok();
        }
    }

    #[test]
    fn empty_zip_is_a_friendly_error() {
        let p = tmp("empty.zip");
        make_zip(&p, zip::CompressionMethod::Stored, &[]);
        let e = open_file(&p).err().unwrap();
        assert_eq!(e.kind, ErrorKind::ImageUnsupported);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff_container(&[0x1f, 0x8b, 8]), Container::Gzip);
        assert_eq!(sniff_container(b"BZh9"), Container::Bzip2);
        assert_eq!(sniff_container(&[0xfd, b'7', b'z', b'X', b'Z', 0]), Container::Xz);
        assert_eq!(sniff_container(&[0x28, 0xb5, 0x2f, 0xfd]), Container::Zstd);
        assert_eq!(sniff_container(b"PK\x03\x04"), Container::Zip);
        assert_eq!(sniff_container(b"hello"), Container::Raw);
    }

    #[test]
    fn raw_file() {
        let p = tmp("raw.img");
        let data = sample(100_000);
        std::fs::write(&p, &data).unwrap();
        let s = open_file(&p).unwrap();
        assert_eq!(s.info.decompressed_size, Some(100_000));
        assert_eq!(read_all(s), data);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn gzip_file_even_with_wrong_extension() {
        let p = tmp("mislabeled.iso");
        let data = sample(300_000);
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&data).unwrap();
        std::fs::write(&p, enc.finish().unwrap()).unwrap();
        let s = open_file(&p).unwrap();
        assert_eq!(s.info.container, Container::Gzip);
        assert_eq!(read_all(s), data);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn xz_size_is_exact() {
        let p = tmp("x.img.xz");
        let data = sample(777_777);
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 1);
        enc.write_all(&data).unwrap();
        std::fs::write(&p, enc.finish().unwrap()).unwrap();
        let s = open_file(&p).unwrap();
        assert_eq!(s.info.container, Container::Xz);
        assert_eq!(s.info.decompressed_size, Some(777_777));
        assert_eq!(read_all(s), data);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn bzip2_and_zstd() {
        let data = sample(200_000);
        let p = tmp("b.bz2");
        let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        enc.write_all(&data).unwrap();
        std::fs::write(&p, enc.finish().unwrap()).unwrap();
        assert_eq!(read_all(open_file(&p).unwrap()), data);
        std::fs::remove_file(&p).ok();

        let p = tmp("z.zst");
        std::fs::write(&p, zstd::encode_all(&data[..], 3).unwrap()).unwrap();
        let s = open_file(&p).unwrap();
        assert_eq!(s.info.container, Container::Zstd);
        assert_eq!(read_all(s), data);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn rejects_virtual_disks() {
        let p = tmp("vm.qcow2");
        std::fs::write(&p, b"QFI\xfb\0\0\0\x03rest").unwrap();
        let e = open_file(&p).err().unwrap();
        assert_eq!(e.kind, ErrorKind::ImageUnsupported);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn missing_file_message() {
        let e = open_file(Path::new("/definitely/not/here.img")).err().unwrap();
        assert_eq!(e.kind, ErrorKind::InvalidInput);
        assert!(e.message.contains("not found"));
    }
}
