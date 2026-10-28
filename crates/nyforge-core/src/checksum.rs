//! Checksums of image files: SHA-256, SHA-1 and MD5.
//!
//! Distributions publish a checksum next to every download so that a damaged or tampered file can be spotted before it
//! is written to a drive. This module reads what people actually have: a bare hash pasted from a web page, a
//! `sha256sum`-style line (`<hash>  <file>`), a BSD-style line (`SHA256 (file) = <hash>`), or a whole `SHA256SUMS` file
//! with many images in it. The algorithm is told by the length of the hash.
//!
//! The file is hashed exactly as it is on disk (so for `foo.iso.xz` that is the `.xz`), which is what the published
//! numbers describe.

use crate::error::{ErrorKind, NyError, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algo {
    Sha256,
    Sha1,
    Md5,
}

impl Algo {
    pub fn name(self) -> &'static str {
        match self {
            Algo::Sha256 => "SHA-256",
            Algo::Sha1 => "SHA-1",
            Algo::Md5 => "MD5",
        }
    }

    /// Number of hexadecimal characters in a hash of this kind.
    pub fn hex_len(self) -> usize {
        match self {
            Algo::Sha256 => 64,
            Algo::Sha1 => 40,
            Algo::Md5 => 32,
        }
    }

    pub fn from_hex_len(n: usize) -> Option<Algo> {
        [Algo::Sha256, Algo::Sha1, Algo::Md5].into_iter().find(|a| a.hex_len() == n)
    }

    /// `sha256`, `sha-1`, `MD5`, … (case and dashes ignored).
    pub fn from_name(s: &str) -> Option<Algo> {
        match s.trim().to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "sha256" => Some(Algo::Sha256),
            "sha1" => Some(Algo::Sha1),
            "md5" => Some(Algo::Md5),
            _ => None,
        }
    }
}

/// A hash somebody published, ready to be compared with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub algo: Algo,
    /// Lower-case hexadecimal.
    pub hex: String,
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn base_name(s: &str) -> &str {
    let s = s.trim().trim_start_matches("./");
    s.rsplit(['/', '\\']).next().unwrap_or(s)
}

/// One hash found in some text, with the file name it was listed for (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    algo: Algo,
    hex: String,
    name: Option<String>,
}

fn parse_line(line: &str) -> Option<Found> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with("-----") || line.starts_with("Hash:") {
        return None;
    }
    // BSD style: SHA256 (file name) = hash
    if let (Some(open), Some(close)) = (line.find(" ("), line.rfind(") = ")) {
        if open < close {
            if let Some(algo) = Algo::from_name(&line[..open]) {
                let hex = line[close + 4..].trim();
                if is_hex(hex) && hex.len() == algo.hex_len() {
                    return Some(Found { algo, hex: hex.to_ascii_lowercase(), name: Some(line[open + 2..close].to_string()) });
                }
            }
        }
    }
    // "sha256: hash" or "SHA-256 hash"
    let mut rest = line;
    let mut named_algo = None;
    for sep in [':', ' ', '='] {
        if let Some((a, b)) = rest.split_once(sep) {
            if let Some(algo) = Algo::from_name(a) {
                named_algo = Some(algo);
                rest = b.trim();
                break;
            }
        }
    }
    // GNU style: hash, then a blank (and maybe a `*` for binary mode), then the name.
    let first = rest.split_whitespace().next().unwrap_or("");
    if is_hex(first) {
        if let Some(algo) = Algo::from_hex_len(first.len()) {
            if named_algo.is_none_or(|n| n == algo) {
                let name = rest[first.len()..].trim().trim_start_matches('*').trim();
                return Some(Found { algo, hex: first.to_ascii_lowercase(), name: if name.is_empty() { None } else { Some(name.to_string()) } });
            }
        }
    }
    // A hash that was copied with spaces or colons inside (some web pages group it): 'ab cd ef …'
    let squeezed: String = rest.chars().filter(|c| !c.is_whitespace() && *c != ':').collect();
    if is_hex(&squeezed) {
        if let Some(algo) = Algo::from_hex_len(squeezed.len()) {
            return Some(Found { algo, hex: squeezed.to_ascii_lowercase(), name: None });
        }
    }
    None
}

/// Find the hash that belongs to `file_name` in `text`.
///
/// `text` may be a bare hash, one line, or a list. With a list, the line naming `file_name` wins; without a name to
/// match, a list with exactly one entry is used.
pub fn parse_expected(text: &str, file_name: Option<&str>) -> Option<Expected> {
    let found: Vec<Found> = text.lines().filter_map(parse_line).collect();
    if found.is_empty() {
        // The whole text may be one hash broken over several lines.
        return parse_line(&text.split_whitespace().collect::<Vec<_>>().join("")).map(|f| Expected { algo: f.algo, hex: f.hex });
    }
    let pick = |f: &Found| Some(Expected { algo: f.algo, hex: f.hex.clone() });
    if let Some(want) = file_name {
        let want = base_name(want);
        if let Some(f) = found.iter().find(|f| f.name.as_deref().is_some_and(|n| base_name(n) == want)) {
            return pick(f);
        }
        if let Some(f) = found.iter().find(|f| f.name.as_deref().is_some_and(|n| base_name(n).eq_ignore_ascii_case(want))) {
            return pick(f);
        }
    }
    match found.as_slice() {
        [only] if only.name.is_none() || file_name.is_none() => pick(only),
        _ => None,
    }
}

/// A checksum file that sits next to the image, when there is one: `x.iso.sha256`, `x.iso.sha256sum`, `SHA256SUMS`, …
pub fn find_beside(image: &Path) -> Option<(PathBuf, Expected)> {
    let dir = image.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = image.file_name()?.to_string_lossy().into_owned();
    let mut candidates: Vec<PathBuf> = vec![];
    for ext in ["sha256", "sha256sum", "sha1", "sha1sum", "md5", "md5sum"] {
        candidates.push(dir.join(format!("{name}.{ext}")));
        if let Some(stem) = image.file_stem() {
            candidates.push(dir.join(format!("{}.{ext}", stem.to_string_lossy())));
        }
    }
    for list in ["SHA256SUMS", "sha256sums", "sha256sums.txt", "SHA256SUMS.txt", "sha256sum.txt", "SHA1SUMS", "sha1sums", "MD5SUMS", "md5sums", "md5sum.txt"] {
        candidates.push(dir.join(list));
    }
    for c in candidates {
        // Only small text files: a checksum list is never large, and this must never read an ISO by mistake.
        let Ok(meta) = std::fs::metadata(&c) else { continue };
        if !meta.is_file() || meta.len() > 256 * 1024 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&c) else { continue };
        if let Some(e) = parse_expected(&text, Some(&name)) {
            return Some((c, e));
        }
    }
    None
}

enum Hasher {
    Sha256(sha2::Sha256),
    Sha1(sha1::Sha1),
    Md5(md5::Md5),
}

impl Hasher {
    fn new(a: Algo) -> Self {
        use sha2::Digest;
        match a {
            Algo::Sha256 => Hasher::Sha256(sha2::Sha256::new()),
            Algo::Sha1 => Hasher::Sha1(sha1::Sha1::new()),
            Algo::Md5 => Hasher::Md5(md5::Md5::new()),
        }
    }

    fn update(&mut self, b: &[u8]) {
        use sha2::Digest;
        match self {
            Hasher::Sha256(h) => h.update(b),
            Hasher::Sha1(h) => h.update(b),
            Hasher::Md5(h) => h.update(b),
        }
    }

    fn finish(self) -> String {
        use sha2::Digest;
        let bytes: Vec<u8> = match self {
            Hasher::Sha256(h) => h.finalize().to_vec(),
            Hasher::Sha1(h) => h.finalize().to_vec(),
            Hasher::Md5(h) => h.finalize().to_vec(),
        };
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Hash of a file's bytes as lower-case hex. `progress(done, total)` is called about every 8 MiB.
pub fn hash_file(path: &Path, algo: Algo, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<String> {
    let mut f = crate::sys::open_sequential(path).map_err(|e| NyError::from_io(&e, &format!("opening {}", path.display())))?;
    let total = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut h = Hasher::new(algo);
    const BLOCK: usize = 4 << 20;
    let (mut done, mut since) = (0u64, 0u64);
    let multi = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) > 1;
    if !multi {
        let mut buf = vec![0u8; BLOCK];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(NyError::cancelled());
            }
            let n = match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(NyError::from_io(&e, &format!("reading {}", path.display()))),
            };
            h.update(&buf[..n]);
            done += n as u64;
            since += n as u64;
            if since >= 8 << 20 {
                since = 0;
                progress(done, total);
            }
        }
        progress(done, total);
        return Ok(h.finish());
    }
    // Reading the next block while the previous one is being hashed: the disk and the CPU work at the same time.
    // Two buffers travel round a pair of channels, so nothing is allocated per block.
    let (full_tx, full_rx) = crossbeam_channel::bounded::<(Vec<u8>, usize)>(2);
    let (free_tx, free_rx) = crossbeam_channel::bounded::<Vec<u8>>(3);
    for _ in 0..3 {
        let _ = free_tx.send(vec![0u8; BLOCK]);
    }
    let stop = AtomicBool::new(false);
    let mut failure: Option<NyError> = None;
    std::thread::scope(|sc| {
        let stop = &stop;
        let reader = sc.spawn(move || -> Option<NyError> {
            while let Ok(mut buf) = free_rx.recv() {
                if stop.load(Ordering::Relaxed) {
                    return None;
                }
                let n = loop {
                    match f.read(&mut buf) {
                        Ok(n) => break n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Some(NyError::from_io(&e, &format!("reading {}", path.display()))),
                    }
                };
                if n == 0 || full_tx.send((buf, n)).is_err() {
                    return None;
                }
            }
            None
        });
        while let Ok((buf, n)) = full_rx.recv() {
            if cancel.load(Ordering::Relaxed) {
                failure = Some(NyError::cancelled());
                break;
            }
            h.update(&buf[..n]);
            done += n as u64;
            since += n as u64;
            if since >= 8 << 20 {
                since = 0;
                progress(done, total);
            }
            let _ = free_tx.send(buf);
        }
        stop.store(true, Ordering::Relaxed);
        drop(free_tx);
        drop(full_rx);
        if let Ok(Some(e)) = reader.join() {
            failure.get_or_insert(e);
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }
    progress(done, total);
    Ok(h.finish())
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub algo: Algo,
    pub actual: String,
    pub expected: String,
    pub matches: bool,
}

/// Hash `path` with the algorithm `expected` calls for and compare.
pub fn check_file(path: &Path, expected: &Expected, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<CheckResult> {
    let actual = hash_file(path, expected.algo, cancel, progress)?;
    Ok(CheckResult { algo: expected.algo, matches: actual == expected.hex, actual, expected: expected.hex.clone() })
}

/// The error for a failed comparison, in plain words.
pub fn mismatch_error(r: &CheckResult) -> NyError {
    NyError::new(
        ErrorKind::ImageCorrupt,
        format!("The checksum does not match. The file's {} is {}, but {} was expected.", r.algo.name(), r.actual, r.expected),
    )
    .with_hint("The download is damaged or is not the file the checksum was made for. Download it again before writing it.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const SHA1_ABC: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
    const MD5_ABC: &str = "900150983cd24fb0d6963f7d28e17f72";

    fn tmp(name: &str, data: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("nyforge-checksum-{}-{name}", std::process::id()));
        std::fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn hashes_match_the_published_test_vectors() {
        let p = tmp("abc", b"abc");
        let c = AtomicBool::new(false);
        assert_eq!(hash_file(&p, Algo::Sha256, &c, &mut |_, _| {}).unwrap(), SHA256_ABC);
        assert_eq!(hash_file(&p, Algo::Sha1, &c, &mut |_, _| {}).unwrap(), SHA1_ABC);
        assert_eq!(hash_file(&p, Algo::Md5, &c, &mut |_, _| {}).unwrap(), MD5_ABC);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_bare_hash_is_enough_whatever_its_case_and_spacing() {
        let e = parse_expected(&SHA256_ABC.to_ascii_uppercase(), None).unwrap();
        assert_eq!((e.algo, e.hex.as_str()), (Algo::Sha256, SHA256_ABC));
        assert_eq!(parse_expected(&format!("  {SHA1_ABC}\n"), Some("x.iso")).unwrap().algo, Algo::Sha1);
        let grouped: Vec<String> = MD5_ABC.as_bytes().chunks(8).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
        assert_eq!(parse_expected(&grouped.join(" "), None).unwrap().hex, MD5_ABC);
        assert_eq!(parse_expected(&format!("sha256: {SHA256_ABC}"), None).unwrap().hex, SHA256_ABC);
    }

    #[test]
    fn sha256sum_and_bsd_lines_are_understood() {
        let gnu = format!("{SHA256_ABC}  archlinux-2026.10.01-x86_64.iso\n");
        assert_eq!(parse_expected(&gnu, Some("archlinux-2026.10.01-x86_64.iso")).unwrap().hex, SHA256_ABC);
        let bin = format!("{SHA1_ABC} *disk.img");
        assert_eq!(parse_expected(&bin, Some("disk.img")).unwrap().algo, Algo::Sha1);
        let bsd = format!("SHA256 (debian.iso) = {SHA256_ABC}");
        assert_eq!(parse_expected(&bsd, Some("debian.iso")).unwrap().hex, SHA256_ABC);
    }

    #[test]
    fn a_list_picks_the_line_for_this_file_and_refuses_to_guess() {
        let other = "0".repeat(64);
        let list = format!("# comment\n{other}  a.iso\n{SHA256_ABC}  ./sub/b.iso\n");
        assert_eq!(parse_expected(&list, Some("b.iso")).unwrap().hex, SHA256_ABC);
        assert_eq!(parse_expected(&list, Some("B.ISO")).unwrap().hex, SHA256_ABC);
        assert!(parse_expected(&list, Some("c.iso")).is_none(), "must not take another file's hash");
        assert!(parse_expected(&list, None).is_none());
        // A named single entry for a different file is not a match either.
        assert!(parse_expected(&format!("{SHA256_ABC}  other.iso"), Some("mine.iso")).is_none());
    }

    #[test]
    fn rubbish_is_not_a_checksum() {
        for s in ["", "hello", "abc", &"g".repeat(64), &"a".repeat(63), &"a".repeat(65)] {
            assert!(parse_expected(s, None).is_none(), "{s:?}");
        }
    }

    #[test]
    fn check_reports_match_and_mismatch() {
        let p = tmp("chk", b"abc");
        let c = AtomicBool::new(false);
        let good = parse_expected(SHA256_ABC, None).unwrap();
        assert!(check_file(&p, &good, &c, &mut |_, _| {}).unwrap().matches);
        let bad = parse_expected(&"1".repeat(64), None).unwrap();
        let r = check_file(&p, &bad, &c, &mut |_, _| {}).unwrap();
        assert!(!r.matches);
        assert!(mismatch_error(&r).message.contains(SHA256_ABC));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_checksum_file_beside_the_image_is_found() {
        let d = std::env::temp_dir().join(format!("nyforge-beside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let iso = d.join("x.iso");
        std::fs::write(&iso, b"abc").unwrap();
        assert!(find_beside(&iso).is_none());
        std::fs::write(d.join("SHA256SUMS"), format!("{}  y.iso\n{SHA256_ABC}  x.iso\n", "2".repeat(64))).unwrap();
        let (path, e) = find_beside(&iso).unwrap();
        assert!(path.ends_with("SHA256SUMS"));
        assert_eq!(e.hex, SHA256_ABC);
        // A file named exactly for the image wins.
        std::fs::write(d.join("x.iso.sha1"), format!("{SHA1_ABC}  x.iso\n")).unwrap();
        assert_eq!(find_beside(&iso).unwrap().1.algo, Algo::Sha1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn cancel_stops_hashing() {
        let p = tmp("cancel", &vec![7u8; 1 << 20]);
        let e = hash_file(&p, Algo::Sha256, &AtomicBool::new(true), &mut |_, _| {}).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Cancelled);
        let _ = std::fs::remove_file(p);
    }
}
