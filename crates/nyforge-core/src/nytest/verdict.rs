//! From numbers to statements. Each finding says what was seen and what it means; nothing is claimed that was not tested.

use super::engine::Op;
use super::{Finding, Report, Severity, Verdict};
use crate::format::human_size;
use serde::{Deserialize, Serialize};

/// What the measured speeds would earn a memory card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdClass {
    /// "Class 10 / U1", "U3 / V30", …
    pub speed_class: Option<String>,
    /// "A1" or "A2"
    pub app_class: Option<String>,
    pub seq_write_mbps: f64,
    pub rnd_read_iops: f64,
    pub rnd_write_iops: f64,
}

/// Speed class from the sustained sequential write speed (MB/s).
pub fn speed_class(mbps: f64) -> Option<&'static str> {
    Some(if mbps >= 90.0 {
        "V90"
    } else if mbps >= 60.0 {
        "V60"
    } else if mbps >= 30.0 {
        "U3 / V30"
    } else if mbps >= 10.0 {
        "Class 10 / U1"
    } else if mbps >= 6.0 {
        "V6 / Class 6"
    } else if mbps >= 4.0 {
        "Class 4"
    } else if mbps >= 2.0 {
        "Class 2"
    } else {
        return None;
    })
}

pub fn app_class(read_iops: f64, write_iops: f64) -> Option<&'static str> {
    if read_iops >= 4000.0 && write_iops >= 2000.0 {
        Some("A2")
    } else if read_iops >= 1500.0 && write_iops >= 500.0 {
        Some("A1")
    } else {
        None
    }
}

fn find<'a>(r: &'a Report, profile: &str, op: Op) -> Option<&'a super::speed::SpeedResult> {
    r.speed.iter().find(|x| x.profile == profile && x.op == op)
}

fn add(r: &mut Vec<Finding>, sev: Severity, id: &str, title: impl Into<String>, detail: impl Into<String>) {
    r.push(Finding { severity: sev, id: id.into(), title: title.into(), detail: detail.into() });
}

pub fn assess(r: &mut Report) {
    let mut out: Vec<Finding> = vec![];

    // ---- is it what it says it is?
    if let Some(i) = &r.integrity {
        let c = &i.report;
        let blocks = c.tested_bytes.div_ceil(if c.mode == crate::usbformat::capacity::CapacityTest::Full { 1 << 20 } else { 4096 }).max(1);
        if c.genuine {
            let how = if c.mode == crate::usbformat::capacity::CapacityTest::Full { "Every block of the drive was written and read back correctly." } else { "A spot check spread over the whole drive found no problem (it is a sampling test: a full check is exact)." };
            add(&mut out, Severity::Good, "capacity_ok", format!("The drive holds all {} it claims", human_size(c.claimed)), how);
        } else if c.mode == crate::usbformat::capacity::CapacityTest::Full && c.bad_blocks * 100 < blocks {
            add(&mut out, Severity::Bad, "bad_blocks", format!("{} bad block{} found", c.bad_blocks, if c.bad_blocks == 1 { "" } else { "s" }), "A few scattered blocks did not hold their data: the flash is wearing out. Do not keep the only copy of anything on this drive.");
        } else {
            add(&mut out, Severity::Bad, "fake_capacity", format!("Fake capacity: it claims {} but holds about {}", human_size(c.claimed), human_size(c.estimated_real)), "Data written past the real size is lost or overwrites other data, silently. Return it. If you keep it, format it to its real size first (NyFormat can do that).");
        }
    }

    // ---- is it readable?
    if let Some(s) = &r.scan {
        if s.error_blocks > 0 {
            add(&mut out, Severity::Bad, "unreadable", format!("{} could not be read", human_size(s.bad_bytes)), format!("{} block{} failed{}. Copy anything you need off this drive now, and do not trust it with new data.", s.error_blocks, if s.error_blocks == 1 { "" } else { "s" }, s.first_error.map(|o| format!(", the first at {} into the drive", human_size(o))).unwrap_or_default()));
        } else if s.slow_blocks > 0 && s.slow_blocks * 100 >= s.blocks.max(1) {
            add(&mut out, Severity::Warning, "slow_areas", format!("{} of the blocks read very slowly", s.slow_blocks), "Areas that answer many times slower than the rest are usually where the drive is struggling to read worn flash. Back it up and watch it.");
        } else if s.slow_blocks > 0 {
            add(&mut out, Severity::Info, "slow_blocks", format!("{} slow block{}", s.slow_blocks, if s.slow_blocks == 1 { "" } else { "s" }), "A few slow reads happen on healthy drives too (the drive was busy with housekeeping). A repeat scan that finds them in the same place would be a warning.");
        } else {
            add(&mut out, Severity::Good, "readable", if s.quick { "No read problems in the sampled blocks" } else { "The whole drive reads cleanly" }, format!("{} read at {:.0} MB/s on average; the slowest 1% of blocks read at {:.0} MB/s.", human_size(s.scanned_bytes), s.avg_mbps, s.min_mbps));
        }
    }

    // ---- what the drive says about itself
    if let Some(sm) = r.info.as_ref().and_then(|i| i.smart.as_ref()) {
        if sm.available {
            if sm.passed == Some(false) {
                add(&mut out, Severity::Bad, "smart_failed", "The drive reports that it is failing (SMART)", "Its own health check failed. Replace it, and copy off what you need first.");
            } else if sm.pending.unwrap_or(0) > 0 || sm.media_errors.unwrap_or(0) > 0 {
                add(&mut out, Severity::Warning, "smart_errors", "The drive has logged errors", format!("{} sectors waiting to be reallocated, {} media errors. Back it up.", sm.pending.unwrap_or(0), sm.media_errors.unwrap_or(0)));
            } else if sm.reallocated.unwrap_or(0) > 0 {
                add(&mut out, Severity::Info, "smart_reallocated", format!("{} sectors have been replaced by spares", sm.reallocated.unwrap_or(0)), "A few is normal over a drive's life; a number that keeps growing is not.");
            } else if sm.passed == Some(true) {
                add(&mut out, Severity::Good, "smart_ok", "The drive reports itself healthy (SMART)", sm.temperature_c.map(|t| format!("Temperature {t} °C.")).unwrap_or_default());
            }
            if sm.wear_percent.unwrap_or(0) >= 90 {
                add(&mut out, Severity::Warning, "worn", format!("{}% of the drive's rated life is used", sm.wear_percent.unwrap_or(0)), "It is close to the end of its rated writes.");
            }
            if sm.temperature_c.unwrap_or(0) >= 70 {
                add(&mut out, Severity::Warning, "hot", format!("The drive is hot ({} °C)", sm.temperature_c.unwrap_or(0)), "Heat makes drives slow down to protect themselves, and wears them faster.");
            }
        }
    }
    if let Some(life) = r.info.as_ref().and_then(|i| i.card.as_ref()).and_then(|c| c.life_time.clone()) {
        if life.contains("0x0a") || life.contains("0x0b") {
            add(&mut out, Severity::Warning, "emmc_worn", "The memory chip reports it is nearly worn out", life);
        }
    }

    // ---- the link
    let seq_read = find(r, "SEQ1M Q8T1", Op::Read).or_else(|| find(r, "SEQ1M Q1T1", Op::Read)).map(|x| x.mbps);
    if let (Some(l), Some(rd)) = (r.info.as_ref().and_then(|i| i.link.clone()), seq_read) {
        if let (Some(speed), Some(ceil)) = (l.speed_mbps, l.ceiling_mbps) {
            if speed <= 480.0 && speed > 12.0 && rd >= ceil * 0.8 {
                add(&mut out, Severity::Warning, "usb2_limit", "The drive is limited by its USB connection", format!("It is connected at {} and read at {:.0} MB/s, which is about all that link can carry. If the drive supports USB 3 (often a blue port or a USB-C port), a different port or a better cable can make it several times faster.", l.label, rd));
            } else if speed >= 5000.0 {
                add(&mut out, Severity::Good, "usb3", format!("Connected at {}", l.label), l.protocol.map(|p| format!("Transfer protocol: {p}.")).unwrap_or_default());
            } else if speed <= 12.0 {
                add(&mut out, Severity::Warning, "usb1", format!("Connected at only {}", l.label), "That is a very slow link: try another port or cable.");
            }
        }
    }

    // ---- speed behaviour
    if let Some(s) = &r.sustained {
        if let Some(c) = &s.cliff {
            let sev = if c.after_mbps < 5.0 { Severity::Warning } else { Severity::Info };
            add(&mut out, sev, "write_cliff", format!("Writes slow down after {}", human_size(c.at_bytes)), format!("The drive writes at {:.0} MB/s into its fast cache, then {:.0} MB/s. A big copy takes much longer than a short benchmark suggests.", c.before_mbps, c.after_mbps));
        } else if s.bytes >= (1 << 30) {
            add(&mut out, Severity::Good, "write_steady", "Writes stayed steady", format!("{} written at {:.0} MB/s on average with no drop.", human_size(s.bytes), s.avg_mbps));
        }
    }
    if let Some(w) = find(r, "RND4K Q1T1", Op::Write) {
        if w.iops < 100.0 && w.mbps < 0.5 {
            add(&mut out, Severity::Info, "slow_small_writes", "Small writes are very slow", format!("Writing 4 KB at a time managed {:.0} operations per second ({:.2} MB/s). That is typical of cheap sticks and cards; it makes running programs or an operating system from it painful.", w.iops, w.mbps));
        }
        if w.lat.p99_us > 500_000 {
            add(&mut out, Severity::Info, "latency_spikes", "Some writes took half a second or more", format!("The slowest 1% of small writes took over {} ms. The drive stalls while it tidies up.", w.lat.p99_us / 1000));
        }
    }

    // ---- memory-card ratings
    let seq_write = r.sustained.as_ref().map(|s| s.end_mbps).filter(|v| *v > 0.0).or_else(|| find(r, "SEQ1M Q8T1", Op::Write).map(|x| x.mbps));
    let rr = find(r, "RND4K Q32T1", Op::Read).map(|x| x.iops);
    let rw = find(r, "RND4K Q32T1", Op::Write).map(|x| x.iops);
    // only for what can be a card or a stick: a rating on an internal disk or a virtual drive would mean nothing
    let card_like = r.info.as_ref().map_or(true, |d| matches!(d.bus.as_str(), "sd" | "mmc"));
    if let (true, Some(sw), Some(rr), Some(rw)) = (card_like, seq_write, rr, rw) {
        r.sd_class = Some(SdClass { speed_class: speed_class(sw).map(String::from), app_class: app_class(rr, rw).map(String::from), seq_write_mbps: sw, rnd_read_iops: rr, rnd_write_iops: rw });
    }

    r.findings = out;

    // ---- the headline
    let worst = r.findings.iter().map(|f| f.severity).max().unwrap_or(Severity::Good);
    let bad: Vec<&Finding> = r.findings.iter().filter(|f| f.severity == Severity::Bad).collect();
    let warn = r.findings.iter().filter(|f| f.severity == Severity::Warning).count();
    let only_speed = r.integrity.is_none() && r.scan.is_none() && r.info.as_ref().and_then(|i| i.smart.as_ref()).map_or(true, |s| !s.available);
    let headline = match worst {
        Severity::Bad => format!("Do not trust this drive: {}.", bad.iter().map(|f| f.title.to_lowercase()).collect::<Vec<_>>().join("; ")),
        Severity::Warning => format!("Works, but {} thing{} to look at.", warn, if warn == 1 { "" } else { "s" }),
        _ if !r.skipped.is_empty() => format!("{} could not run, so this is not a full check.{}", r.skipped.join(" and "), if matches!(worst, Severity::Good | Severity::Info) { " What did run found nothing wrong." } else { "" }),
        _ if only_speed => "No problems seen. Only speed was measured: run the surface scan and capacity check to know the drive's health.".to_string(),
        _ => "No problems found in the tests that were run.".to_string(),
    };
    r.verdict = Verdict { level: match worst { Severity::Info => Severity::Good, s => s }, headline };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nytest::engine::{Access, Latency};
    use crate::nytest::speed::SpeedResult;

    fn empty() -> Report {
        Report {
            tool: "t".into(),
            started: "x".into(),
            elapsed_secs: 1.0,
            target: "/dev/x".into(),
            mode: "file".into(),
            steps: vec![],
            info: None,
            speed: vec![],
            sustained: None,
            scan: None,
            integrity: None,
            sd_class: None,
            findings: vec![],
            verdict: Verdict { level: Severity::Good, headline: String::new() },
            notes: vec![],
            skipped: vec![],
        }
    }

    fn sp(profile: &str, op: Op, mbps: f64, iops: f64) -> SpeedResult {
        SpeedResult { name: format!("{profile}"), profile: profile.into(), op, access: Access::Random, block: 4096, qd: 1, mbps, iops, runs: vec![mbps], lat: Latency::default() }
    }

    #[test]
    fn classes() {
        assert_eq!(speed_class(95.0), Some("V90"));
        assert_eq!(speed_class(31.0), Some("U3 / V30"));
        assert_eq!(speed_class(12.0), Some("Class 10 / U1"));
        assert_eq!(speed_class(1.0), None);
        assert_eq!(app_class(4500.0, 2100.0), Some("A2"));
        assert_eq!(app_class(1600.0, 600.0), Some("A1"));
        assert_eq!(app_class(1600.0, 100.0), None);
    }

    #[test]
    fn a_fake_drive_is_called_bad() {
        let mut r = empty();
        r.integrity = Some(crate::nytest::integrity::IntegrityResult {
            report: crate::usbformat::capacity::assess(crate::usbformat::capacity::CapacityTest::Full, 64 << 30, 64 << 30, &(0..100u64).map(|i| (8 << 30) + i * (1 << 20)).collect::<Vec<_>>(), 1 << 20),
            write_mbps: 10.0,
            read_mbps: 20.0,
            seconds: 5.0,
        });
        assert_eq!(r.integrity.as_ref().unwrap().report.genuine, false);
        assert_eq!(r.integrity.as_ref().unwrap().report.bad_blocks, 100);
        assess(&mut r);
        assert_eq!(r.verdict.level, Severity::Bad);
        assert!(r.findings.iter().any(|f| f.id == "fake_capacity" || f.id == "bad_blocks"));
        assert!(r.verdict.headline.starts_with("Do not trust"));
    }

    #[test]
    fn speed_only_is_honest_about_what_it_covers() {
        let mut r = empty();
        r.speed = vec![sp("SEQ1M Q8T1", Op::Read, 90.0, 90.0), sp("SEQ1M Q8T1", Op::Write, 40.0, 40.0)];
        assess(&mut r);
        assert_eq!(r.verdict.level, Severity::Good);
        assert!(r.verdict.headline.contains("Only speed was measured"), "{}", r.verdict.headline);
    }

    #[test]
    fn usb2_ceiling_is_pointed_out() {
        let mut r = empty();
        r.info = Some(crate::nytest::info::DeviceInfo {
            name: "x".into(),
            size: 1,
            bus: "usb".into(),
            removable: true,
            details: vec![],
            link: Some(crate::nytest::info::LinkInfo { kind: "USB".into(), speed_mbps: Some(480.0), label: "USB 2.0 High-Speed (480 Mb/s)".into(), ceiling_mbps: Some(42.0), protocol: None }),
            smart: None,
            card: None,
        });
        r.speed = vec![sp("SEQ1M Q8T1", Op::Read, 38.0, 38.0)];
        assess(&mut r);
        assert!(r.findings.iter().any(|f| f.id == "usb2_limit"));
        assert_eq!(r.verdict.level, Severity::Warning);
        // a slow stick on a USB 2 port is just slow, not "limited by the port"
        r.speed = vec![sp("SEQ1M Q8T1", Op::Read, 12.0, 12.0)];
        assess(&mut r);
        assert!(!r.findings.iter().any(|f| f.id == "usb2_limit"));
    }

    #[test]
    fn card_classes_come_from_the_measurements() {
        let mut r = empty();
        r.speed = vec![sp("SEQ1M Q8T1", Op::Write, 35.0, 35.0), sp("RND4K Q32T1", Op::Read, 2000.0, 2000.0), sp("RND4K Q32T1", Op::Write, 700.0, 700.0)];
        assess(&mut r);
        let c = r.sd_class.unwrap();
        assert_eq!(c.speed_class.as_deref(), Some("U3 / V30"));
        assert_eq!(c.app_class.as_deref(), Some("A1"));
    }
}
