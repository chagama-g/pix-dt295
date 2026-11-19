//! The frequency plan: the ISDB-T channels (UHF 13-62ch, CATV C13-C63) and the BS /
//! CS110 transponders behind the LNB.

use std::ops::RangeInclusive;

/// UHF channel numbers.
pub const UHF: RangeInclusive<u32> = 13..=62;
/// CATV channel numbers (C13-C63).
pub const CATV: RangeInclusive<u32> = 13..=63;
/// The IF range of the satellite tuner (HORUS3A), in kHz.
pub const SAT_IF_KHZ: RangeInclusive<u32> = 950_000..=2_150_000;

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

/// The LNB's local oscillator for BS / CS110 (right-hand circular): a transponder's
/// frequency minus its IF.
pub const LNB_LO_KHZ: u32 = 10_678_000;
/// IF of BS-1 and the step per odd (right-hand) transponder.
const BS_IF_BASE_KHZ: u32 = 1_049_480;
const BS_IF_STEP_KHZ: u32 = 38_360;
/// IF of ND02 and the step per even (right-hand) ND.
const ND_IF_BASE_KHZ: u32 = 1_613_000;
const ND_IF_STEP_KHZ: u32 = 40_000;

/// The IF of BS-`n` in kHz: the right-hand transponders BS-1, BS-3, ... BS-23. `None`
/// for an even one (left-hand, the 4K broadcasts: its IF is past the tuner's range).
pub const fn bs_if_khz(n: u16) -> Option<u32> {
    if n >= 1 && n <= 23 && n % 2 == 1 {
        Some(BS_IF_BASE_KHZ + (n as u32 - 1) / 2 * BS_IF_STEP_KHZ)
    } else {
        None
    }
}

/// The IF of CS110 ND`n` in kHz: the right-hand ND02, ND04, ... ND24. `None` for an odd
/// one (left-hand).
pub const fn nd_if_khz(n: u16) -> Option<u32> {
    if n >= 2 && n <= 24 && n.is_multiple_of(2) {
        Some(ND_IF_BASE_KHZ + (n as u32 - 2) / 2 * ND_IF_STEP_KHZ)
    } else {
        None
    }
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
    fn satellite_plan() {
        assert_eq!(bs_if_khz(1), Some(1_049_480));
        assert_eq!(bs_if_khz(15), Some(1_318_000));
        assert_eq!(bs_if_khz(23), Some(1_471_440));
        assert_eq!(nd_if_khz(2), Some(1_613_000));
        assert_eq!(nd_if_khz(24), Some(2_053_000));
        // left-hand (and out of range): none
        for n in [0, 2, 22, 24, 25] {
            assert_eq!(bs_if_khz(n), None, "BS-{n}");
        }
        for n in [0, 1, 3, 23, 26] {
            assert_eq!(nd_if_khz(n), None, "ND{n}");
        }
        // every right-hand one is within the tuner's IF range
        let all = (1..=23)
            .filter_map(bs_if_khz)
            .chain((2..=24).filter_map(nd_if_khz));
        let all: Vec<u32> = all.collect();
        assert_eq!(all.len(), 12 + 12);
        for khz in all {
            assert!(SAT_IF_KHZ.contains(&khz), "{khz} kHz");
        }
        assert_eq!(
            bs_if_khz(15).unwrap() + LNB_LO_KHZ,
            11_996_000,
            "BS-15 at 11996 MHz"
        );
    }
}
