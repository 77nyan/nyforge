//! Small formatting helpers shared by every front end.

/// Decimal sizes, like the numbers printed on the drive packaging.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[i])
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

pub fn human_speed(bytes_per_sec: f64) -> String {
    format!("{}/s", human_size(bytes_per_sec.max(0.0) as u64))
}

pub fn human_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--:--".into();
    }
    let s = secs.round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// Parse "4G", "512M", "1.5GiB", "1000000" (binary units: K/M/G/T mean 1024-based).
pub fn parse_size(s: &str) -> Option<u64> {
    let t = s.trim().to_ascii_lowercase();
    let t = t.strip_suffix("ib").or_else(|| t.strip_suffix('b')).unwrap_or(&t);
    let (num, mult) = match t.chars().last()? {
        'k' => (&t[..t.len() - 1], 1u64 << 10),
        'm' => (&t[..t.len() - 1], 1 << 20),
        'g' => (&t[..t.len() - 1], 1 << 30),
        't' => (&t[..t.len() - 1], 1 << 40),
        c if c.is_ascii_digit() => (t, 1),
        _ => return None,
    };
    let v: f64 = num.trim().parse().ok()?;
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    Some((v * mult as f64) as u64)
}

pub fn round_up(v: u64, align: u64) -> u64 {
    if align == 0 {
        return v;
    }
    v.div_ceil(align) * align
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1_000), "1.00 KB");
        assert_eq!(human_size(15_931_539_456), "15.9 GB");
        assert_eq!(human_size(512_110_190_592), "512 GB");
    }

    #[test]
    fn durations() {
        assert_eq!(human_duration(59.4), "00:59");
        assert_eq!(human_duration(3725.0), "1:02:05");
        assert_eq!(human_duration(f64::NAN), "--:--");
    }

    #[test]
    fn rounding() {
        assert_eq!(round_up(1, 512), 512);
        assert_eq!(round_up(512, 512), 512);
        assert_eq!(round_up(513, 512), 1024);
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("4G"), Some(4 << 30));
        assert_eq!(parse_size("512m"), Some(512 << 20));
        assert_eq!(parse_size("1.5GiB"), Some(3 << 29));
        assert_eq!(parse_size("1000"), Some(1000));
        assert_eq!(parse_size("abc"), None);
        assert_eq!(parse_size("-3G"), None);
    }
}
