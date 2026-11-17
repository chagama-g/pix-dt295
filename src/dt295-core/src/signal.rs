//! C/N of the TC90522 raw demod values, in 0.001 dB, with the polynomial fits of the Linux tc90522
//! driver:
//! * ISDB-T (3 B @ 0x8B): P = 10 log10(5505024 / raw),
//!   C/N = 0.024 P^4 - 1.6 P^3 + 39.8 P^2 + 549.1 P + 3096.5
//! * ISDB-S (2 B @ 0xBC): P = sqrt(raw - 3000) / 64,
//!   C/N = -1634.6 P^5 + 14341 P^4 - 50259 P^3 + 88977 P^2 - 89565 P + 58857

/// ISDB-T C/N (0.001 dB) of the 3-byte value at demod register 0x8B.
pub fn cn_t_mdb(raw: u32) -> i32 {
    if raw == 0 {
        return 0;
    }
    let p = 10.0 * (5_505_024.0 / f64::from(raw)).log10();
    let cn = (((0.024 * p - 1.6) * p + 39.8) * p + 549.1) * p + 3096.5;
    if cn > 0.0 { cn as i32 } else { 0 }
}

/// ISDB-S C/N (0.001 dB) of the 2-byte value at demod register 0xBC.
pub fn cn_s_mdb(raw: u32) -> i32 {
    if raw < 3000 {
        return 0;
    }
    let p = f64::from(raw - 3000).sqrt() / 64.0;
    let cn = ((((-1634.6 * p + 14341.0) * p - 50259.0) * p + 88977.0) * p - 89565.0) * p + 58857.0;
    if cn > 0.0 { cn as i32 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known C/N values for these raw values.
    #[test]
    fn known_values() {
        let t = [
            (0, 0),
            (1, 226_399),
            (100, 69_329),
            (5000, 32_139),
            (20_000, 25_451),
            (50_000, 21_452),
            (100_000, 18_479),
            (200_000, 15_508),
            (400_000, 12_550),
            (800_000, 9666),
            (1_600_000, 6961),
            (3_000_000, 4792),
            (5_505_024, 3096),
            (8_000_000, 2317),
            (16_777_215, 1565),
        ];
        for (raw, want) in t {
            assert_eq!(cn_t_mdb(raw), want, "T raw {raw}");
        }
        let s = [
            (0, 0),
            (2999, 0),
            (3000, 58_857),
            (3001, 57_479),
            (3100, 46_851),
            (4000, 31_069),
            (6000, 22_813),
            (10_000, 17_189),
            (20_000, 10_391),
            (30_000, 5618),
            (40_000, 0),
            (50_000, 0),
            (60_000, 0),
            (65_535, 0),
        ];
        for (raw, want) in s {
            assert_eq!(cn_s_mdb(raw), want, "S raw {raw}");
        }
    }
}
