//! Partition tables: a plain MBR, and GPT with its protective MBR, primary and backup copies.

use crate::error::{NyError, Result};

pub const SECTOR: u64 = 512;
pub const MIB_SECTORS: u64 = 2048;
/// GPT: 128 entries of 128 bytes = 32 sectors, after the header.
pub const GPT_ENTRY_SECTORS: u64 = 32;
/// Sectors a GPT needs at the end of the disk (backup entries + backup header).
pub const GPT_TAIL_SECTORS: u64 = GPT_ENTRY_SECTORS + 1;
pub const GPT_FIRST_USABLE: u64 = 2 + GPT_ENTRY_SECTORS;

pub type Guid = [u8; 16];

/// Parse "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" into the on-disk byte order
/// (the first three groups are stored little-endian).
pub fn guid(s: &str) -> Guid {
    let hex: Vec<u8> = s.bytes().filter(|b| *b != b'-').collect();
    assert_eq!(hex.len(), 32, "bad GUID {s}");
    let byte = |i: usize| u8::from_str_radix(std::str::from_utf8(&hex[i * 2..i * 2 + 2]).unwrap(), 16).unwrap();
    let mut g = [0u8; 16];
    for i in 0..16 {
        g[i] = byte(i);
    }
    g[0..4].reverse();
    g[4..6].reverse();
    g[6..8].reverse();
    g
}

pub fn random_guid() -> Guid {
    let mut g = [0u8; 16];
    let _ = getrandom::fill(&mut g);
    g[7] = (g[7] & 0x0F) | 0x40; // version 4
    g[8] = (g[8] & 0x3F) | 0x80; // RFC 4122 variant
    g
}

/// Microsoft basic data: FAT, exFAT, NTFS.
pub fn type_basic_data() -> Guid {
    guid("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7")
}
/// Linux filesystem data.
pub fn type_linux() -> Guid {
    guid("0FC63DAF-8483-4772-8E79-3D69D8477DE4")
}

#[derive(Debug, Clone)]
pub struct TablePart {
    pub start: u64,
    pub sectors: u64,
    pub mbr_type: u8,
    pub gpt_type: Guid,
    pub unique: Guid,
    pub name: String,
}

/// CHS bytes for an LBA, with the usual 255 heads / 63 sectors; "too big" is 0xFE 0xFF 0xFF.
fn chs(lba: u64) -> [u8; 3] {
    let c = lba / (255 * 63);
    if c > 1023 {
        return [0xFE, 0xFF, 0xFF];
    }
    let h = (lba / 63) % 255;
    let s = lba % 63 + 1;
    [h as u8, (s as u8) | (((c >> 8) as u8) << 6), (c & 0xFF) as u8]
}

/// A plain MBR (no boot code: these are data drives).
pub fn mbr_sector(disk_signature: u32, parts: &[TablePart]) -> Result<[u8; 512]> {
    if parts.len() > 4 {
        return Err(NyError::invalid("An MBR partition table holds at most 4 partitions. Use --scheme gpt for more."));
    }
    let mut s = [0u8; 512];
    s[440..444].copy_from_slice(&disk_signature.to_le_bytes());
    for (i, p) in parts.iter().enumerate() {
        if p.start > u32::MAX as u64 || p.sectors > u32::MAX as u64 || p.start + p.sectors > (u32::MAX as u64) + 1 {
            return Err(NyError::invalid("An MBR partition cannot start or end beyond 2 TiB. Use --scheme gpt for bigger drives."));
        }
        let o = 446 + i * 16;
        s[o] = 0x00;
        s[o + 1..o + 4].copy_from_slice(&chs(p.start));
        s[o + 4] = p.mbr_type;
        s[o + 5..o + 8].copy_from_slice(&chs(p.start + p.sectors - 1));
        s[o + 8..o + 12].copy_from_slice(&(p.start as u32).to_le_bytes());
        s[o + 12..o + 16].copy_from_slice(&(p.sectors as u32).to_le_bytes());
    }
    s[510] = 0x55;
    s[511] = 0xAA;
    Ok(s)
}

/// The MBR a GPT disk carries so old tools see "one big partition of unknown type" instead of free space.
pub fn protective_mbr(disk_sectors: u64) -> [u8; 512] {
    let mut s = [0u8; 512];
    let o = 446;
    s[o + 1..o + 4].copy_from_slice(&[0x00, 0x02, 0x00]);
    s[o + 4] = 0xEE;
    s[o + 5..o + 8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    s[o + 8..o + 12].copy_from_slice(&1u32.to_le_bytes());
    let size = (disk_sectors - 1).min(u32::MAX as u64) as u32;
    s[o + 12..o + 16].copy_from_slice(&size.to_le_bytes());
    s[510] = 0x55;
    s[511] = 0xAA;
    s
}

pub struct Gpt {
    pub protective_mbr: [u8; 512],
    pub primary_header: [u8; 512],
    /// 32 sectors, written at LBA 2.
    pub entries: Vec<u8>,
    pub backup_header: [u8; 512],
    /// Same bytes as `entries`, written at `disk_sectors - 33`.
    pub backup_entries_lba: u64,
    pub backup_header_lba: u64,
}

fn header(my: u64, alt: u64, entries_lba: u64, disk_sectors: u64, disk_guid: &Guid, entries_crc: u32) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&my.to_le_bytes());
    h[32..40].copy_from_slice(&alt.to_le_bytes());
    h[40..48].copy_from_slice(&GPT_FIRST_USABLE.to_le_bytes());
    h[48..56].copy_from_slice(&(disk_sectors - GPT_TAIL_SECTORS - 1).to_le_bytes());
    h[56..72].copy_from_slice(disk_guid);
    h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    h[80..84].copy_from_slice(&128u32.to_le_bytes());
    h[84..88].copy_from_slice(&128u32.to_le_bytes());
    h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let crc = crc32fast::hash(&h[..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

pub fn build_gpt(disk_sectors: u64, disk_guid: Guid, parts: &[TablePart]) -> Result<Gpt> {
    if parts.len() > 128 {
        return Err(NyError::invalid("A GPT holds at most 128 partitions."));
    }
    if disk_sectors < GPT_FIRST_USABLE + GPT_TAIL_SECTORS + 1 {
        return Err(NyError::invalid("The drive is too small for a GPT partition table."));
    }
    let last_usable = disk_sectors - GPT_TAIL_SECTORS - 1;
    let mut entries = vec![0u8; (GPT_ENTRY_SECTORS * SECTOR) as usize];
    for (i, p) in parts.iter().enumerate() {
        if p.start < GPT_FIRST_USABLE || p.start + p.sectors - 1 > last_usable {
            return Err(NyError::other("internal error: partition outside the GPT usable area"));
        }
        let e = &mut entries[i * 128..(i + 1) * 128];
        e[0..16].copy_from_slice(&p.gpt_type);
        e[16..32].copy_from_slice(&p.unique);
        e[32..40].copy_from_slice(&p.start.to_le_bytes());
        e[40..48].copy_from_slice(&(p.start + p.sectors - 1).to_le_bytes());
        for (j, u) in p.name.encode_utf16().take(36).enumerate() {
            e[56 + j * 2..58 + j * 2].copy_from_slice(&u.to_le_bytes());
        }
    }
    let crc = crc32fast::hash(&entries);
    let last = disk_sectors - 1;
    Ok(Gpt {
        protective_mbr: protective_mbr(disk_sectors),
        primary_header: header(1, last, 2, disk_sectors, &disk_guid, crc),
        entries,
        backup_header: header(last, 1, disk_sectors - GPT_TAIL_SECTORS, disk_sectors, &disk_guid, crc),
        backup_entries_lba: disk_sectors - GPT_TAIL_SECTORS,
        backup_header_lba: last,
    })
}

/// Read a table back (tests and `nyforge format --list` style checks): `(first, last, type, name)` per used entry.
pub fn parse_gpt_header_and_entries(header_sector: &[u8], entries: &[u8]) -> Option<Vec<(u64, u64, Guid, String)>> {
    if header_sector.len() < 92 || &header_sector[0..8] != b"EFI PART" {
        return None;
    }
    let mut h = header_sector[..92].to_vec();
    let crc = u32::from_le_bytes(h[16..20].try_into().unwrap());
    h[16..20].fill(0);
    if crc32fast::hash(&h) != crc {
        return None;
    }
    let n = u32::from_le_bytes(header_sector[80..84].try_into().unwrap()) as usize;
    let sz = u32::from_le_bytes(header_sector[84..88].try_into().unwrap()) as usize;
    let ecrc = u32::from_le_bytes(header_sector[88..92].try_into().unwrap());
    if entries.len() < n * sz || crc32fast::hash(&entries[..n * sz]) != ecrc {
        return None;
    }
    let mut out = vec![];
    for i in 0..n {
        let e = &entries[i * sz..(i + 1) * sz];
        if e[0..16].iter().all(|b| *b == 0) {
            continue;
        }
        let units: Vec<u16> = e[56..128].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|u| *u != 0).collect();
        out.push((
            u64::from_le_bytes(e[32..40].try_into().unwrap()),
            u64::from_le_bytes(e[40..48].try_into().unwrap()),
            e[0..16].try_into().unwrap(),
            String::from_utf16_lossy(&units),
        ));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(start: u64, sectors: u64, name: &str) -> TablePart {
        TablePart { start, sectors, mbr_type: 0x0C, gpt_type: type_basic_data(), unique: random_guid(), name: name.into() }
    }

    #[test]
    fn guid_byte_order_matches_the_spec() {
        // EBD0A0A2-B9E5-4433-87C0-68B6B72699C7 is stored as A2 A0 D0 EB E5 B9 33 44 87 C0 68 B6 B7 26 99 C7
        assert_eq!(type_basic_data(), [0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7]);
        let g = random_guid();
        assert_eq!(g[7] >> 4, 4);
        assert_eq!(g[8] >> 6, 2);
    }

    #[test]
    fn mbr_has_signature_entries_and_chs() {
        let s = mbr_sector(0xDEADBEEF, &[part(2048, 100_000, "a"), part(102_048, 5000, "b")]).unwrap();
        assert_eq!(&s[510..], &[0x55, 0xAA]);
        assert_eq!(&s[440..444], &0xDEADBEEFu32.to_le_bytes());
        assert_eq!(s[446 + 4], 0x0C);
        assert_eq!(u32::from_le_bytes(s[446 + 8..446 + 12].try_into().unwrap()), 2048);
        assert_eq!(u32::from_le_bytes(s[446 + 12..446 + 16].try_into().unwrap()), 100_000);
        assert_eq!(&s[446 + 1..446 + 4], &[32, 33, 0]); // LBA 2048 = C0 H32 S33, as parted writes it
        assert_eq!(u32::from_le_bytes(s[462 + 8..462 + 12].try_into().unwrap()), 102_048);
        assert_eq!(chs(u32::MAX as u64), [0xFE, 0xFF, 0xFF]);
    }

    #[test]
    fn mbr_refuses_five_partitions_and_the_far_side_of_2_tib() {
        let five: Vec<_> = (0..5).map(|i| part(2048 + i * 4096, 4096, "x")).collect();
        assert!(mbr_sector(1, &five).is_err());
        assert!(mbr_sector(1, &[part(2048, u32::MAX as u64, "big")]).is_err());
        assert!(mbr_sector(1, &[part(1 << 32, 100, "far")]).is_err());
    }

    #[test]
    fn protective_mbr_covers_the_disk() {
        let s = protective_mbr(100_000);
        assert_eq!(s[446 + 4], 0xEE);
        assert_eq!(u32::from_le_bytes(s[446 + 12..446 + 16].try_into().unwrap()), 99_999);
        assert_eq!(protective_mbr(1 << 40)[446 + 12..446 + 16], u32::MAX.to_le_bytes());
    }

    #[test]
    fn gpt_both_copies_verify_and_round_trip() {
        let disk = 200_000u64;
        let parts = vec![part(2048, 100_000, "Data"), part(102_048, 50_000, "Linux ✓")];
        let g = build_gpt(disk, random_guid(), &parts).unwrap();
        let primary = parse_gpt_header_and_entries(&g.primary_header, &g.entries).unwrap();
        let backup = parse_gpt_header_and_entries(&g.backup_header, &g.entries).unwrap();
        assert_eq!(primary, backup);
        assert_eq!(primary.len(), 2);
        assert_eq!(primary[0].0, 2048);
        assert_eq!(primary[0].1, 2048 + 100_000 - 1);
        assert_eq!(primary[1].3, "Linux ✓");
        assert_eq!(g.backup_header_lba, disk - 1);
        assert_eq!(g.backup_entries_lba, disk - 33);
        // header fields point where the spec says
        assert_eq!(u64::from_le_bytes(g.primary_header[32..40].try_into().unwrap()), disk - 1);
        assert_eq!(u64::from_le_bytes(g.backup_header[24..32].try_into().unwrap()), disk - 1);
        assert_eq!(u64::from_le_bytes(g.backup_header[72..80].try_into().unwrap()), disk - 33);
        assert_eq!(u64::from_le_bytes(g.primary_header[48..56].try_into().unwrap()), disk - 34);
        // a flipped bit is noticed
        let mut bad = g.primary_header;
        bad[40] ^= 1;
        assert!(parse_gpt_header_and_entries(&bad, &g.entries).is_none());
        let mut bad_entries = g.entries.clone();
        bad_entries[33] ^= 1;
        assert!(parse_gpt_header_and_entries(&g.primary_header, &bad_entries).is_none());
    }

    #[test]
    fn gpt_rejects_partitions_in_the_reserved_areas() {
        let disk = 200_000u64;
        assert!(build_gpt(disk, random_guid(), &[part(10, 100, "x")]).is_err());
        assert!(build_gpt(disk, random_guid(), &[part(2048, disk, "x")]).is_err());
    }
}
