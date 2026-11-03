//! CRC-32/MPEG-2 for PSI sections.

use crc::{CRC_32_MPEG_2, Crc};

const MPEG2: Crc<u32> = Crc::<u32>::new(&CRC_32_MPEG_2);

/// The shortest long-form section: 8 header bytes and the CRC_32.
const LONG_MIN: usize = 12;

/// CRC-32/MPEG-2 of `data` (a whole section including its CRC field yields 0).
pub fn crc32(data: &[u8]) -> u32 {
    MPEG2.checksum(data)
}

/// Whether the last four bytes of `sec` are the CRC of the bytes before them
/// (`false` for fewer than four bytes).
pub(crate) fn trailing_crc_ok(sec: &[u8]) -> bool {
    match sec.split_last_chunk::<4>() {
        Some((body, crc)) => crc32(body) == u32::from_be_bytes(*crc),
        None => false,
    }
}

/// Whether a long-form section (table_id .. CRC_32) has a valid CRC.
pub fn section_crc_ok(sec: &[u8]) -> bool {
    sec.len() >= LONG_MIN && trailing_crc_ok(sec)
}

/// Recompute the trailing CRC_32 of a section in place.
pub fn set_section_crc(sec: &mut [u8]) {
    if let Some((body, crc)) = sec.split_last_chunk_mut::<4>() {
        *crc = crc32(body).to_be_bytes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain bitwise MPEG CRC-32, as the reference for the crate.
    fn mpeg_crc(b: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &x in b {
            crc ^= u32::from(x) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 {
                    (crc << 1) ^ 0x04C1_1DB7
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    #[test]
    fn check_value_and_c_reference() {
        assert_eq!(crc32(b"123456789"), 0x0376_E6E7);
        assert_eq!(crc32(&[]), 0xFFFF_FFFF);
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        for n in [0, 1, 3, 4, 17, 183, 1000] {
            assert_eq!(crc32(&data[..n]), mpeg_crc(&data[..n]), "len {n}");
        }
    }

    #[test]
    fn set_then_check() {
        let mut sec = [
            0x00, 0xB0, 0x0D, 0x7F, 0xE8, 0xC1, 0, 0, 0, 0, 0xE0, 0x10, 0, 0, 0, 0,
        ];
        assert!(!section_crc_ok(&sec));
        set_section_crc(&mut sec);
        assert!(section_crc_ok(&sec));
        assert_eq!(crc32(&sec), 0, "a whole section including its CRC yields 0");
        sec[5] ^= 1;
        assert!(!section_crc_ok(&sec));
    }

    #[test]
    fn short_input() {
        assert!(!section_crc_ok(&[]));
        let mut tiny = [1u8, 2, 3];
        set_section_crc(&mut tiny);
        assert_eq!(tiny, [1, 2, 3]);
        assert!(!trailing_crc_ok(&tiny));
        // 4..11 bytes: a valid CRC, but too short for a long-form section
        let mut short = [0x42u8, 0, 0, 0, 0];
        set_section_crc(&mut short);
        assert!(trailing_crc_ok(&short));
        assert!(!section_crc_ok(&short));
    }
}
