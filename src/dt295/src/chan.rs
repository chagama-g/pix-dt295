//! Channel numbers shared by the BonDriver and the `dt295` command: the built-in
//! ISDB-T tables (UHF 13-62ch, CATV C13-C63) and the value of a satellite ini line
//! (`IF_kHz,TSID[,ServiceID]`, read with the number syntax of C `wcstoul(.., 0)`).

use std::ops::RangeInclusive;

/// TSID of a tune that selects none (ISDB-T, or a satellite line without one).
pub const NO_TSID: u16 = 0xFFFF;
/// UHF channel numbers.
pub const UHF: RangeInclusive<u32> = 13..=62;
/// CATV channel numbers (C13-C63).
pub const CATV: RangeInclusive<u32> = 13..=63;
/// The IF range of the satellite tuner (HORUS3A), in kHz.
const SAT_IF_KHZ: RangeInclusive<u32> = 950_000..=2_150_000;

/// Centre frequency of UHF channel `n` (ISDB-T: band centre + 1/7 MHz).
pub fn uhf_hz(n: u32) -> Option<u32> {
    UHF.contains(&n).then(|| 473_142_857 + (n - 13) * 6_000_000)
}

/// Centre frequency of CATV channel C`n` (6 MHz bands, the vendor's table): C13-C21 at
/// 108-162 MHz, C22 at 164-170 MHz (not next to C21), C23-C63 at 222-468 MHz.
pub fn catv_hz(n: u32) -> Option<u32> {
    CATV.contains(&n).then(|| match n {
        ..=21 => 111_142_857 + (n - 13) * 6_000_000,
        22 => 167_142_857,
        _ => 225_142_857 + (n - 23) * 6_000_000,
    })
}

/// The value of a `[BS]` / `[CS110]` ini line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SatLine {
    /// The IF in Hz.
    pub freq_hz: u32,
    /// [`NO_TSID`] when the line has none.
    pub tsid: u16,
    /// `None` when omitted or negative (every service of the TS).
    pub sid: Option<u16>,
}

/// `IF_kHz,TSID[,ServiceID]`; `None` for an IF outside 950-2150 MHz.
pub fn satellite_line(value: &str) -> Option<SatLine> {
    let (khz, rest) = strtoul(value);
    let mut tsid = u32::from(NO_TSID);
    let mut sid: i32 = -1;
    if let Some(rest) = rest.strip_prefix(',') {
        let (t, rest) = strtoul(rest);
        tsid = t;
        if let Some(rest) = rest.strip_prefix(',') {
            sid = strtol(rest).0;
        }
    }
    SAT_IF_KHZ.contains(&khz).then(|| SatLine {
        freq_hz: khz * 1000,
        tsid: tsid as u16, // truncated to 16 bits
        // the device takes a 16-bit service id
        sid: (sid >= 0).then_some((sid & 0xFFFF) as u16),
    })
}

/// C `wcstoul(s, &end, 0)` with a 32-bit `unsigned long`: leading whitespace, an
/// optional sign (`-` negates modulo 2^32), `0x` = hex, leading `0` = octal, else
/// decimal; saturates at `u32::MAX`. Returns the value and the unparsed rest (the
/// whole input when no digits were found).
pub fn strtoul(s: &str) -> (u32, &str) {
    match parse_c_integer(s) {
        Some((neg, mag, rest)) => {
            let v = match u32::try_from(mag) {
                Ok(v) if neg => v.wrapping_neg(),
                Ok(v) => v,
                Err(_) => u32::MAX, // ERANGE, either sign
            };
            (v, rest)
        }
        None => (0, s),
    }
}

/// C `wcstol(s, &end, 0)` with a 32-bit `long` (saturating at `i32::MIN/MAX`).
pub fn strtol(s: &str) -> (i32, &str) {
    match parse_c_integer(s) {
        Some((neg, mag, rest)) => {
            let v = if neg {
                i32::try_from(-i128::from(mag)).unwrap_or(i32::MIN)
            } else {
                i32::try_from(mag).unwrap_or(i32::MAX)
            };
            (v, rest)
        }
        None => (0, s),
    }
}

/// Sign, saturated magnitude and rest of a base-0 C integer; `None` without digits.
fn parse_c_integer(s: &str) -> Option<(bool, u64, &str)> {
    let t = s.trim_start_matches(|c: char| c.is_whitespace());
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let b = t.as_bytes();
    let (radix, digits) =
        if b.len() > 2 && b[0] == b'0' && (b[1] | 0x20) == b'x' && b[2].is_ascii_hexdigit() {
            (16, &t[2..])
        } else if b.first() == Some(&b'0') {
            (8, t)
        } else {
            (10, t)
        };
    let end = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let mag = digits[..end].chars().fold(0u64, |acc, c| {
        let d = u64::from(c.to_digit(radix).unwrap_or(0));
        acc.saturating_mul(u64::from(radix)).saturating_add(d)
    });
    Some((neg, mag, &digits[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrestrial_tables() {
        assert_eq!(uhf_hz(13), Some(473_142_857));
        assert_eq!(uhf_hz(27), Some(557_142_857));
        assert_eq!(uhf_hz(62), Some(767_142_857));
        assert_eq!((uhf_hz(12), uhf_hz(63)), (None, None));
        assert_eq!(catv_hz(13), Some(111_142_857));
        assert_eq!(catv_hz(21), Some(159_142_857));
        assert_eq!(catv_hz(22), Some(167_142_857), "164-170 MHz");
        assert_eq!(catv_hz(23), Some(225_142_857));
        assert_eq!(catv_hz(63), Some(465_142_857));
        assert_eq!((catv_hz(12), catv_hz(64)), (None, None));
    }

    #[test]
    fn satellite_lines() {
        let l = satellite_line("1318000,0x40F1,101").unwrap();
        assert_eq!(
            (l.freq_hz, l.tsid, l.sid),
            (1_318_000_000, 0x40F1, Some(101))
        );
        let l = satellite_line("1733000,0x6080").unwrap();
        assert_eq!((l.tsid, l.sid), (0x6080, None));
        assert_eq!(satellite_line("1733000,0x6080,-1").unwrap().sid, None);
        assert_eq!(satellite_line("1049480").unwrap().tsid, NO_TSID);
        assert!(satellite_line("949999,0x4010").is_none());
        assert!(satellite_line("2150001,0x4010").is_none());
    }
}
