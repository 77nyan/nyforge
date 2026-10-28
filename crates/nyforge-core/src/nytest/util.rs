//! Small helpers the tests share: sizes in words, the time, free space.

/// `4G`, `700M`, `1.5GiB` or `4096` (bytes). Powers of 1024.
pub fn parse_size(text: &str) -> Option<u64> {
    let t = text.trim().to_ascii_lowercase();
    let t = t.trim_end_matches('b').trim_end_matches('i');
    let (num, mult) = match t.chars().last()? {
        'k' => (&t[..t.len() - 1], 1u64 << 10),
        'm' => (&t[..t.len() - 1], 1 << 20),
        'g' => (&t[..t.len() - 1], 1 << 30),
        't' => (&t[..t.len() - 1], 1 << 40),
        c if c.is_ascii_digit() => (t, 1),
        _ => return None,
    };
    let v: f64 = num.trim().parse().ok()?;
    (v.is_finite() && v > 0.0).then(|| (v * mult as f64) as u64)
}

/// `2026-10-28T12:34:56Z` for a Unix time.
pub fn iso8601(unix: u64) -> String {
    let days = (unix / 86400) as i64;
    let rem = unix % 86400;
    // civil-from-days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn now_iso() -> String {
    iso8601(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
}

/// Free space on the file system that holds `path` (its folder when the file does not exist yet).
pub fn free_space(path: &std::path::Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let dir = if path.is_dir() { path } else { path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(std::path::Path::new(".")) };
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        return Some(s.f_bavail as u64 * s.f_frsize as u64);
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}


#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;

    pub fn tdir(name: &str) -> PathBuf {
        let base = std::env::var_os("NYFORGE_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("nyforge-nytest-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
