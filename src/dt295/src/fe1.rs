//! System1 front end -- demod + tuners: the exact captured vendor I2C sequences,
//! provided by a third party. It feeds the MB86M01 the TS
//! the System1 stream path expects (the MxL603-derived System2 front end locked but
//! configured the demod TS output differently, so cmd(5,0,2) underran to 0xFFFF).
//!
//! Demod addresses: ISDB-T 0x20, ISDB-S 0x22. The tuners are reached through the demod
//! I2C gateway: write `[FE C0 reg data...]`; read via `[FE C0 FB reg]` then `[FE C1]`.

use tracing::{info, warn};

use crate::proto::{FW_STALL_LIMIT, Io, sleep_ms};
use crate::signal::{cn_s_mdb, cn_t_mdb};
use crate::{Error, Result, Status, System, i2c};

const DEMOD_T: u8 = 0x20;
const DEMOD_S: u8 = 0x22;

/// The I2C bus as the front end sees it (the device's type-9 I2C, or a test recorder).
pub(crate) trait Bus {
    fn i2c_write(&mut self, slave: u8, data: &[u8]) -> Result<()>;
    fn i2c_write_read(&mut self, slave: u8, wdata: &[u8], rbuf: &mut [u8]) -> Result<()>;
    fn sleep_ms(&mut self, ms: u64);
    /// The firmware stopped acking: every transfer fails from now on.
    fn wedged(&self) -> bool {
        false
    }
}

impl Bus for Io<'_> {
    fn i2c_write(&mut self, slave: u8, data: &[u8]) -> Result<()> {
        i2c::write(self, slave, data)
    }
    fn i2c_write_read(&mut self, slave: u8, wdata: &[u8], rbuf: &mut [u8]) -> Result<()> {
        i2c::write_read(self, slave, wdata, rbuf)
    }
    fn sleep_ms(&mut self, ms: u64) {
        sleep_ms(ms);
    }
    fn wedged(&self) -> bool {
        self.p.fw_stall >= FW_STALL_LIMIT
    }
}

/// A tune whose firmware stopped acking did not happen: an error at once, not a lock
/// wait of seconds (a recorder gives its tuner command ~16 s, and the host recovers the
/// device on this error).
fn not_wedged(b: &impl Bus) -> Result<()> {
    if b.wedged() {
        Err(Error::Timeout("firmware wedged (not acking commands)"))
    } else {
        Ok(())
    }
}

// ---- captured init tables ----

#[rustfmt::skip]
const DEMOD_T_INIT: [[u8; 2]; 22] = [
    [0x01, 0x00], [0x03, 0x90], [0x06, 0x80], [0x1C, 0x20], [0x1D, 0x56], [0x1E, 0x6A],
    [0x1F, 0x9A], [0x23, 0x4D], [0x25, 0x00], [0x26, 0x00], [0x32, 0x21], [0x3B, 0x21],
    [0x3C, 0x3F], [0x70, 0x38], [0x73, 0x45], [0x74, 0x11], [0x75, 0x02], [0x76, 0x00],
    [0x77, 0x01], [0x7D, 0xD2], [0xE4, 0xF4], [0xF4, 0x80],
];
#[rustfmt::skip]
const DEMOD_S_INIT: [[u8; 2]; 19] = [
    [0x01, 0x90], [0x03, 0x00], [0x04, 0x02], [0x07, 0x33], [0x08, 0x11], [0x10, 0xB0],
    [0x11, 0x00], [0x13, 0x80], [0x15, 0x00], [0x17, 0x01], [0x1C, 0x59], [0x1D, 0xAA],
    [0x1E, 0x20], [0x1F, 0x19], [0x85, 0x7A], [0x87, 0x04], [0x8E, 0x26], [0xA3, 0x91],
    [0xA5, 0x40],
];
const TUNER_S_PARK: [[u8; 2]; 5] = [
    [0x2A, 0xB9],
    [0x29, 0x70],
    [0x28, 0x3E],
    [0x2A, 0x19],
    [0x1C, 0x00],
];

/// One tuner-table op.
#[derive(Clone, Copy)]
enum TOp {
    /// Raw `[FE C0 FF]`.
    Raw,
    /// Tuner write reg = value.
    W(u8, u8),
    /// Read the register (verify, the value is discarded; `old` is the expected one),
    /// then write `new`.
    M { reg: u8, _old: u8, new: u8 },
}

use TOp::{M, Raw, W};

#[rustfmt::skip]
const TUNER_T_INIT: &[TOp] = &[
    Raw,
    W(0x00, 0x01), W(0x96, 0x00), W(0x00, 0x00), W(0x14, 0x13), W(0x6D, 0x8A), W(0x6D, 0x0A),
    W(0xDF, 0x19), W(0x45, 0x1B), W(0xA9, 0x59), W(0xAA, 0x6A), W(0xBE, 0x4C), W(0xCF, 0x2A),
    W(0xD2, 0xF5), W(0xD0, 0x34), W(0x77, 0xE7), W(0x78, 0xE3), W(0x6F, 0x51), W(0x7B, 0x84),
    W(0x7C, 0x9F), W(0x56, 0x41), W(0xCD, 0x64), W(0xC3, 0x2C), W(0x9D, 0x61), W(0xF7, 0x52),
    W(0x58, 0x81), W(0xB1, 0x30), W(0x00, 0x01),
    M { reg: 0x31, _old: 0x41, new: 0xD1 },
    W(0x00, 0x00), W(0x01, 0x89), W(0x02, 0x00), W(0x6D, 0x0A), W(0x0B, 0x01),
    M { reg: 0x04, _old: 0x02, new: 0x01 },
    W(0x05, 0x1B),
    M { reg: 0x08, _old: 0x01, new: 0x05 },
    M { reg: 0x09, _old: 0x42, new: 0x42 },
    M { reg: 0x5E, _old: 0x00, new: 0x00 },
    W(0x0A, 0x00), W(0x0C, 0x00), W(0x13, 0x04), W(0x53, 0xFE), W(0x57, 0x91), W(0x5A, 0xF9),
    W(0x5B, 0x18), W(0x5C, 0xF1), W(0x62, 0xC2), W(0x6E, 0x01), W(0x6F, 0x51), W(0x87, 0x77),
    W(0x88, 0x55), W(0x93, 0x22), W(0x97, 0x02), W(0xBA, 0x30), W(0x98, 0xAF), W(0x9B, 0x20),
    W(0x9C, 0x1E), W(0xA0, 0x18), W(0xA5, 0x09), W(0xC2, 0xA9), W(0xC5, 0x7C), W(0xCD, 0xE8),
    W(0xCE, 0x7F), W(0xD5, 0x03), W(0xD9, 0x04), W(0xEA, 0x0D), W(0xDC, 0x1C), W(0x6D, 0x0A),
    W(0x03, 0x00), W(0x03, 0x01),
];

// ---- I2C helpers (results are ignored where the captured sequence ignores them) ----

fn d_write(b: &mut impl Bus, demod: u8, reg: u8, val: u8) -> Result<()> {
    b.i2c_write(demod, &[reg, val])
}

fn d_read(b: &mut impl Bus, demod: u8, reg: u8) -> Result<u8> {
    let mut v = [0u8; 1];
    b.i2c_write_read(demod, &[reg], &mut v)?;
    Ok(v[0])
}

fn t_write(b: &mut impl Bus, demod: u8, regvals: &[u8]) -> Result<()> {
    let mut buf = [0u8; 8];
    buf[0] = 0xFE;
    buf[1] = 0xC0;
    buf[2..2 + regvals.len()].copy_from_slice(regvals);
    b.i2c_write(demod, &buf[..2 + regvals.len()])
}

fn t_read(b: &mut impl Bus, demod: u8, reg: u8) -> Result<u8> {
    let _ = b.i2c_write(demod, &[0xFE, 0xC0, 0xFB, reg]);
    let mut v = [0u8; 1];
    b.i2c_write_read(demod, &[0xFE, 0xC1], &mut v)?;
    Ok(v[0])
}

fn run_tuner_table(b: &mut impl Bus, demod: u8, tbl: &[TOp]) {
    for op in tbl {
        match *op {
            Raw => {
                let _ = b.i2c_write(demod, &[0xFE, 0xC0, 0xFF]);
            }
            W(reg, val) => {
                let _ = t_write(b, demod, &[reg, val]);
            }
            M { reg, new, .. } => {
                let _ = t_read(b, demod, reg);
                let _ = t_write(b, demod, &[reg, new]);
            }
        }
    }
}

fn park_tuner_s(b: &mut impl Bus) {
    for rv in TUNER_S_PARK {
        let _ = t_write(b, DEMOD_S, &rv);
    }
    let _ = t_write(b, DEMOD_S, &[0x11, 0xC0, 0xA7]);
    let _ = d_write(b, DEMOD_S, 0x07, 0x13);
}

/// Tuner 0x10 / 0x11 for a terrestrial frequency: the frequency in 1/64 MHz, low byte
/// first (the vendor driver's MxL603 SetFreq; it accepts 90-770 MHz). A channel centre
/// is an odd number of MHz + 1/7, so the low byte is 0x49 or 0xC9.
fn terr_regs(f_hz: u32) -> (u8, u8) {
    let v = u64::from(f_hz) * 64 / 1_000_000;
    (v as u8, (v >> 8) as u8)
}

/// Tuner 0x31 / 0x7C: the vendor driver switches them at 700 MHz (UHF 51ch and up).
fn terr_band_regs(f_hz: u32) -> (u8, u8) {
    if f_hz >= 700_000_000 {
        (0x91, 0x9F)
    } else {
        (0x81, 0x1F)
    }
}

fn park_s(b: &mut impl Bus) {
    for [r, v] in [[0x1E, 0x28], [0x0A, 0x00], [0x03, 0x01], [0x17, 0x01]] {
        let _ = d_write(b, DEMOD_S, r, v);
    }
    let _ = d_read(b, DEMOD_S, 0x1E);
    let _ = d_write(b, DEMOD_S, 0x1E, 0x38);
    park_tuner_s(b);
}

/// Front-end power-on: the T demod, T tuner, S demod and S tuner init tables, the S
/// tuner parked.
pub(crate) fn power_on_init(b: &mut impl Bus) -> Result<()> {
    info!("front-end init (T/S sequence)");
    for [r, v] in DEMOD_T_INIT {
        let _ = d_write(b, DEMOD_T, r, v);
    }
    run_tuner_table(b, DEMOD_T, TUNER_T_INIT);
    for [r, v] in DEMOD_S_INIT {
        let _ = d_write(b, DEMOD_S, r, v);
    }
    // TUNER_S_INIT: 2A=B9, wn 06=10,10,00, 0A=00, 0E=00
    let _ = t_write(b, DEMOD_S, &[0x2A, 0xB9]);
    let _ = t_write(b, DEMOD_S, &[0x06, 0x10, 0x10, 0x00]);
    let _ = t_write(b, DEMOD_S, &[0x0A, 0x00]);
    let _ = t_write(b, DEMOD_S, &[0x0E, 0x00]);
    park_tuner_s(b);
    Ok(())
}

/// Lock polls in a row with bit 7 of demod reg 0x80 set after which an ISDB-T channel
/// counts as empty (~0.9 s at ~56 ms a poll). Hardware 2026-10-19, a UHF scan: on all
/// 40 empty channels 0x80 went 0x2f -> 0xaf within 0.2 s and stayed there; on the 10
/// present ones bit 7 never showed (0x2f -> 0x2e/0x0e -> 0x06 -> 0x02 -> 0x00, lock in
/// 0.7-1.0 s).
const T_NO_SIGNAL_POLLS: u32 = 16;

fn wait_lock_t(b: &mut impl Bus) {
    let t0 = std::time::Instant::now();
    let mut v = 0u8;
    let mut no_signal = 0;
    // Up to ~3.4 s for a signal that is there but slow to lock. An empty channel gives
    // up after T_NO_SIGNAL_POLLS: a channel scan meets 40 of them on UHF alone.
    for _ in 0..60 {
        if b.wedged() {
            return;
        }
        if let Ok(x) = d_read(b, DEMOD_T, 0x80) {
            v = x;
            no_signal = if x & 0x80 != 0 { no_signal + 1 } else { 0 };
            if no_signal >= T_NO_SIGNAL_POLLS {
                info!(
                    "fe1: T no signal (reg0x80=0x{x:02x}) [{} ms]",
                    t0.elapsed().as_millis()
                );
                return;
            }
            if x == 0x00 {
                info!("fe1: T locked [{} ms]", t0.elapsed().as_millis());
                b.sleep_ms(120);
                let _ = d_write(b, DEMOD_T, 0x1D, 0x02);
                let _ = d_write(b, DEMOD_T, 0x1E, 0x2A);
                let _ = d_write(b, DEMOD_T, 0x1F, 0x8A);
                for r in [0x77, 0x76, 0x5C] {
                    if let Ok(rr) = d_read(b, DEMOD_T, r) {
                        let _ = d_write(b, DEMOD_T, r, rr);
                    }
                }
                return;
            }
        }
        b.sleep_ms(50);
    }
    warn!(
        "fe1: T did not lock (reg0x80=0x{v:02x}) [{} ms]",
        t0.elapsed().as_millis()
    );
}

/// Tune ISDB-T (`freq_hz` = the channel centre) and wait for lock (a missing lock is
/// logged, not an error: the caller reads the status).
pub(crate) fn tune_t(b: &mut impl Bus, freq_hz: u32) -> Result<()> {
    const PRE: [[u8; 2]; 6] = [
        [0x03, 0x00],
        [0x1D, 0xAA],
        [0x1E, 0xAA],
        [0x1F, 0xAA],
        [0x25, 0x00],
        [0x23, 0x4D],
    ];
    let (r10, r11) = terr_regs(freq_hz);
    let (r31, r7c) = terr_band_regs(freq_hz);
    let post = [
        [0x00, 0x01],
        [0x31, r31],
        [0x00, 0x00],
        [0x7C, r7c],
        [0x12, 0x00],
    ];
    info!(
        "fe1: tune T {:.3} MHz (reg10={r10:02x} reg11={r11:02x})",
        f64::from(freq_hz) / 1e6
    );
    park_s(b);
    // Wake the T side the way the vendor does before every terrestrial tune
    // (frontend_all.txt): an ISDB-S tune puts it to sleep (tuner 0x12 = 0, 0x0B = 0,
    // demod 0x03 = 0x90) and without tuner 0x0B = 1 every later ISDB-T tune of the
    // session saw reg 0x80 = 0xaf, no lock (hardware 2026-10-19: a UHF scan after BS/CS).
    let _ = t_write(b, DEMOD_T, &[0x0B, 0x01]);
    let _ = t_write(b, DEMOD_T, &[0x12, 0x01]);
    let _ = d_write(b, DEMOD_T, 0x03, 0x00);
    for [r, v] in PRE {
        let _ = d_write(b, DEMOD_T, r, v);
    }
    let _ = t_read(b, DEMOD_T, 0x0D);
    for rv in [[0x0D, 0x00], [0x0F, 0x20], [0x10, r10], [0x11, r11]] {
        let _ = t_write(b, DEMOD_T, &rv);
    }
    for rv in post {
        let _ = t_write(b, DEMOD_T, &rv);
    }
    let _ = t_read(b, DEMOD_T, 0x6E);
    let _ = t_write(b, DEMOD_T, &[0x6E, 0x81]);
    let _ = t_read(b, DEMOD_T, 0x14);
    let _ = t_write(b, DEMOD_T, &[0x14, 0x13]);
    let _ = t_write(b, DEMOD_T, &[0x12, 0x01]);
    let _ = t_read(b, DEMOD_T, 0x6E);
    let _ = t_write(b, DEMOD_T, &[0x6E, 0x01]);
    let _ = d_read(b, DEMOD_T, 0x0E);
    let _ = d_write(b, DEMOD_T, 0x0E, 0x71);
    let _ = d_write(b, DEMOD_T, 0x0F, 0x10);
    let _ = d_write(b, DEMOD_T, 0x01, 0x40);
    let _ = d_write(b, DEMOD_T, 0x23, 0x4C);
    not_wedged(b)?;
    wait_lock_t(b);
    not_wedged(b)
}

// ---- ISDB-S tune (TUNE_S_SEQ) ----
//
// The captured vendor satellite I2C sequence, replayed with four substitutions:
//   freq (FE C0 00 ..): round(IF_MHz / 0.5), from freq_hz
//   band (FE C0 09 ..): 0x8C for BS, 0x44 for CS
//   reg 0x8F / 0x90:    the broadcast TSID, high / low byte
// freq_hz is the L-band IF (as the ini supplies it); 0x8F / 0x90 are the TC90522
// TS-select registers (Linux tc90522s_set_tsid writes them the same way). The capture
// was for BS-15 / TS1 = TSID 0x40F1. A BS TSID packs 0x4 | renumber count (bits 11-9) |
// transponder (8-4) | relative TS (3-0); CS110 TSIDs start with 0x6 or 0x7. The
// PLL-related tuner writes need an ~80 ms settle or reg 0xC3 never clears (as provided
// by a third party). An op with rd > 0 writes its bytes then reads rd bytes.

struct SOp {
    slave: u8,
    rd: usize,
    data: &'static [u8],
}

const fn s(slave: u8, rd: usize, data: &'static [u8]) -> SOp {
    SOp { slave, rd, data }
}

#[rustfmt::skip]
const TUNE_S_SEQ: &[SOp] = &[
    s(0x20, 0, &[0x25, 0x00]), s(0x20, 0, &[0x23, 0x4d]), s(0x20, 0, &[0x03, 0x90]),
    s(0x20, 0, &[0xfe, 0xc0, 0x12, 0x00]), s(0x20, 0, &[0xfe, 0xc0, 0x0b, 0x00]),
    s(0x22, 1, &[0x1e]),
    s(0x22, 0, &[0x1e, 0x28]), s(0x22, 0, &[0xfe, 0xc0, 0x11, 0x00, 0xa7]),
    s(0x22, 0, &[0xfe, 0xc0, 0x2a, 0xb9]), s(0x22, 0, &[0xfe, 0xc0, 0x1c, 0xc0]),
    s(0x22, 0, &[0xfe, 0xc0, 0x29, 0x71]),
    s(0x22, 0, &[0x17, 0x00]), s(0x22, 0, &[0x1e, 0x20]), s(0x22, 0, &[0x0a, 0xff]),
    s(0x22, 0, &[0x03, 0x01]),
    s(0x22, 0, &[0x1c, 0xaa]), s(0x22, 0, &[0x1f, 0x2a]),
    s(0x22, 0, &[0x8f, 0x40]), s(0x22, 0, &[0x90, 0xf1]),
    s(0x22, 0, &[0x1e, 0x28]), s(0x22, 0, &[0x0a, 0x00]), s(0x22, 0, &[0x03, 0x01]),
    s(0x22, 0, &[0xfe, 0xc0, 0x11, 0x00, 0xa7]),
    s(0x22, 0, &[0xfe, 0xc0, 0x2a, 0xb9]), s(0x22, 0, &[0xfe, 0xc0, 0x1c, 0xc0]),
    s(0x22, 0, &[0xfe, 0xc0, 0x29, 0x71]),
    s(0x22, 0, &[0xfe, 0xc0, 0x00, 0x0a, 0x4c, 0x00, 0x00, 0x00]),
    s(0x22, 0, &[0xfe, 0xc0, 0x09, 0x8c]),
    s(0x22, 0, &[0xfe, 0xc0, 0x37, 0xac]), s(0x22, 0, &[0xfe, 0xc0, 0x05, 0x80]),
    s(0x22, 0, &[0xfe, 0xc0, 0x2a, 0xbb]),
    s(0x20, 0, &[0x0e, 0x77]), s(0x20, 0, &[0x0f, 0x77]),
    s(0x22, 1, &[0x07]),
    s(0x22, 0, &[0x07, 0x11]), s(0x22, 0, &[0x08, 0x10]),
    s(0x22, 2, &[0xfe, 0xc1]),
    s(0x22, 0, &[0xfe, 0xc0, 0x50, 0xf0]),
    s(0x22, 0, &[0x1e, 0x20]), s(0x22, 0, &[0x0a, 0xff]), s(0x22, 0, &[0x03, 0x01]),
    s(0x22, 0, &[0x03, 0x01]),
    s(0x22, 0, &[0x1c, 0x08]), s(0x22, 0, &[0x1f, 0x08]),
    s(0x22, 1, &[0xa5]), s(0x22, 0, &[0xa5, 0x40]),
    s(0x22, 1, &[0xa3]), s(0x22, 0, &[0xa3, 0x80]),
];

fn wait_lock_s(b: &mut impl Bus) -> bool {
    let mut v = 0xFFu8;
    for _ in 0..20 {
        // ~8 s at 0.4 s intervals
        if b.wedged() {
            return false;
        }
        if let Ok(x) = d_read(b, DEMOD_S, 0xC3) {
            v = x;
            if x == 0x00 {
                info!("fe1: S locked");
                return true;
            }
        }
        b.sleep_ms(400);
    }
    warn!("fe1: S no lock (0xC3={v:02x})");
    false
}

/// The TSIDs the transponder carries, from its TMCC (demod S reg 0xCE: eight
/// big-endian slots; 0 and 0xFFFF are empty), in slot order. (hardware 2026-10-29)
pub(crate) fn ts_ids_s(b: &mut impl Bus) -> Result<Vec<u16>> {
    let mut x = [0u8; 16];
    b.i2c_write_read(DEMOD_S, &[0xCE], &mut x)?;
    let (slots, _) = x.as_chunks::<2>();
    Ok(slots
        .iter()
        .map(|&w| u16::from_be_bytes(w))
        .filter(|&t| t != 0 && t != 0xFFFF)
        .collect())
}

/// The TSID the demod streams (demod S reg 0xE6, big-endian; a TSID the transponder
/// does not carry reads as its first TS, the one streamed then; without a TSID,
/// 0xFFFF). (hardware 2026-10-29)
pub(crate) fn ts_id_s(b: &mut impl Bus) -> Result<u16> {
    let mut x = [0u8; 2];
    b.i2c_write_read(DEMOD_S, &[0xE6], &mut x)?;
    Ok(u16::from_be_bytes(x))
}

/// After a satellite lock: the TS the demod streams, which should be `tsid`. The
/// selection can follow the lock by a moment, so it is read again for up to 1 s.
/// Without a TSID ([`NO_TSID`](crate::chan::NO_TSID)) the demod streams the
/// transponder's first TS. Logs the transponder's TSIDs.
fn check_ts_s(b: &mut impl Bus, tsid: u16) -> Option<u16> {
    let none = crate::chan::NO_TSID;
    let mut got = None;
    for _ in 0..10 {
        got = ts_id_s(b).ok();
        if tsid == none || got == Some(tsid) {
            break;
        }
        b.sleep_ms(100);
    }
    let ids = ts_ids_s(b).unwrap_or_default();
    if got == Some(none) {
        got = ids.first().copied();
    }
    let all: Vec<String> = ids.iter().map(|t| format!("{t:04x}")).collect();
    match got {
        Some(t) if t == tsid || tsid == none => {
            info!("fe1: S TS {t:04x} (transponder: {})", all.join(" "));
        }
        _ => warn!(
            "fe1: S streams TS {} instead of {tsid:04x} (transponder: {}): check the TSID",
            got.map_or("-".into(), |t| format!("{t:04x}")),
            all.join(" ")
        ),
    }
    got
}

/// Tune ISDB-S (`freq_hz` = IF, `tsid` = the wanted TS) and wait for lock. Returns the
/// TSID the demod streams, when it could be read.
pub(crate) fn tune_s(b: &mut impl Bus, freq_hz: u32, tsid: u16) -> Result<Option<u16>> {
    let v = ((u64::from(freq_hz) + 250_000) / 500_000) as u32; // round(IF_MHz / 0.5)
    let [r8f, r90] = tsid.to_be_bytes();
    // CS110 TSIDs are 0x6xxx / 0x7xxx (BS: 0x4xxx); an older `r8f == 0x60` test
    // treated e.g. ND4 = 0x7040 as BS.
    let band = if (tsid >> 12) >= 0x6 { 0x44 } else { 0x8C };
    info!(
        "fe1: tune S {:.3} MHz tsid={tsid:04x} (freq={:04x} 8f={r8f:02x} 90={r90:02x})",
        f64::from(freq_hz) / 1e6,
        v & 0xFFFF
    );
    for op in TUNE_S_SEQ {
        let mut d = [0u8; 8];
        let len = op.data.len();
        d[..len].copy_from_slice(op.data);
        let mut settle = 3;
        if op.slave == DEMOD_S && op.rd == 0 {
            match (len, d[0], d[1], d[2]) {
                (8, 0xFE, 0xC0, 0x00) => {
                    d[3] = (v >> 8) as u8;
                    d[4] = v as u8;
                }
                (4, 0xFE, 0xC0, 0x09) => d[3] = band,
                (2, 0x8F, ..) => d[1] = r8f,
                (2, 0x90, ..) => d[1] = r90,
                _ => {}
            }
        }
        if op.rd == 0 {
            let _ = b.i2c_write(op.slave, &d[..len]);
            if op.slave == DEMOD_S
                && len >= 3
                && d[0] == 0xFE
                && d[1] == 0xC0
                && matches!(d[2], 0x00 | 0x1C | 0x29 | 0x2A)
            {
                settle = 80; // PLL settle
            }
        } else {
            let mut rb = [0u8; 4];
            let _ = b.i2c_write_read(op.slave, &d[..len], &mut rb[..op.rd]);
        }
        b.sleep_ms(settle);
    }
    not_wedged(b)?;
    let locked = wait_lock_s(b);
    not_wedged(b)?;
    Ok(if locked { check_ts_s(b, tsid) } else { None })
}

/// ISDB-S demod sample, the registers the vendor driver polls while streaming (as
/// provided by a third party): lock 0xC3 (0 = locked), C/N 0xBC (2 B) and the two 3-byte error
/// counters 0xEB and 0xF0 ("mostly 0, now and then non-zero"). (lock, cn_raw, eb, f0).
pub(crate) fn sample_s(b: &mut impl Bus) -> Result<(u8, u32, u32, u32)> {
    let lock = d_read(b, DEMOD_S, 0xC3)?;
    let mut x = [0u8; 3];
    b.i2c_write_read(DEMOD_S, &[0xBC], &mut x[..2])?;
    let cn = u32::from(u16::from_be_bytes([x[0], x[1]]));
    b.i2c_write_read(DEMOD_S, &[0xEB], &mut x)?;
    let eb = be24(x);
    b.i2c_write_read(DEMOD_S, &[0xF0], &mut x)?;
    let f0 = be24(x);
    Ok((lock, cn, eb, f0))
}

/// ISDB-T demod sample for the live signal level: lock 0x80 (0 = locked) and the 3-byte
/// C/N at 0x8B. (lock, cn_raw).
pub(crate) fn sample_t(b: &mut impl Bus) -> Result<(u8, u32)> {
    let lock = d_read(b, DEMOD_T, 0x80)?;
    let mut x = [0u8; 3];
    b.i2c_write_read(DEMOD_T, &[0x8B], &mut x)?;
    Ok((lock, be24(x)))
}

fn be24(x: [u8; 3]) -> u32 {
    (u32::from(x[0]) << 16) | (u32::from(x[1]) << 8) | u32::from(x[2])
}

/// Lock and C/N right after a tune. The real C/N: the TC90522 CNR is the 3-byte value
/// at demod reg 0x8B (ISDB-T) / the 2-byte value at 0xBC (ISDB-S), run through the
/// polynomial fits (an older reading of the single byte 0x82 as dB showed ~108).
pub(crate) fn read_status(b: &mut impl Bus, sys: System) -> Result<Status> {
    let mut st = Status::default();
    match sys {
        System::Terrestrial => {
            st.locked = d_read(b, DEMOD_T, 0x80).unwrap_or(0xFF) == 0x00;
            let mut cn = [0u8; 3];
            if b.i2c_write_read(DEMOD_T, &[0x8B], &mut cn).is_ok() {
                st.raw = be24(cn);
                st.cn_mdb = cn_t_mdb(st.raw);
            }
        }
        System::Satellite => {
            st.locked = d_read(b, DEMOD_S, 0xC3).unwrap_or(0xFF) == 0x00;
            let mut cn = [0u8; 2];
            if b.i2c_write_read(DEMOD_S, &[0xBC], &mut cn).is_ok() {
                st.raw = u32::from(u16::from_be_bytes(cn));
                st.cn_mdb = cn_s_mdb(st.raw);
            }
        }
    }
    Ok(st)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use super::*;

    /// Records the I2C traffic: lock registers read 0x00, every other read 0x5A.
    #[derive(Default)]
    struct Rec(String);

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    impl Bus for Rec {
        fn i2c_write(&mut self, slave: u8, data: &[u8]) -> Result<()> {
            write!(self.0, " W{slave:02x}={}", hex(data)).unwrap();
            Ok(())
        }
        fn i2c_write_read(&mut self, slave: u8, w: &[u8], r: &mut [u8]) -> Result<()> {
            write!(self.0, " R{slave:02x}={}/{}", hex(w), r.len()).unwrap();
            r.fill(0x5A);
            if w.len() == 1 && ((slave == 0x20 && w[0] == 0x80) || (slave == 0x22 && w[0] == 0xC3))
            {
                r[0] = 0;
            }
            Ok(())
        }
        fn sleep_ms(&mut self, ms: u64) {
            write!(self.0, " S{ms}").unwrap();
        }
    }

    fn rec(f: impl FnOnce(&mut Rec)) -> String {
        let mut r = Rec::default();
        f(&mut r);
        r.0
    }

    // The expected I2C sequences, byte for byte.
    const POWER_ON: &str = " W20=0100 W20=0390 W20=0680 W20=1c20 W20=1d56 W20=1e6a W20=1f9a \
        W20=234d W20=2500 W20=2600 W20=3221 W20=3b21 W20=3c3f W20=7038 W20=7345 W20=7411 W20=7502 \
        W20=7600 W20=7701 W20=7dd2 W20=e4f4 W20=f480 W20=fec0ff W20=fec00001 W20=fec09600 \
        W20=fec00000 W20=fec01413 W20=fec06d8a W20=fec06d0a W20=fec0df19 W20=fec0451b W20=fec0a959 \
        W20=fec0aa6a W20=fec0be4c W20=fec0cf2a W20=fec0d2f5 W20=fec0d034 W20=fec077e7 W20=fec078e3 \
        W20=fec06f51 W20=fec07b84 W20=fec07c9f W20=fec05641 W20=fec0cd64 W20=fec0c32c W20=fec09d61 \
        W20=fec0f752 W20=fec05881 W20=fec0b130 W20=fec00001 W20=fec0fb31 R20=fec1/1 W20=fec031d1 \
        W20=fec00000 W20=fec00189 W20=fec00200 W20=fec06d0a W20=fec00b01 W20=fec0fb04 R20=fec1/1 \
        W20=fec00401 W20=fec0051b W20=fec0fb08 R20=fec1/1 W20=fec00805 W20=fec0fb09 R20=fec1/1 \
        W20=fec00942 W20=fec0fb5e R20=fec1/1 W20=fec05e00 W20=fec00a00 W20=fec00c00 W20=fec01304 \
        W20=fec053fe W20=fec05791 W20=fec05af9 W20=fec05b18 W20=fec05cf1 W20=fec062c2 W20=fec06e01 \
        W20=fec06f51 W20=fec08777 W20=fec08855 W20=fec09322 W20=fec09702 W20=fec0ba30 W20=fec098af \
        W20=fec09b20 W20=fec09c1e W20=fec0a018 W20=fec0a509 W20=fec0c2a9 W20=fec0c57c W20=fec0cde8 \
        W20=fec0ce7f W20=fec0d503 W20=fec0d904 W20=fec0ea0d W20=fec0dc1c W20=fec06d0a W20=fec00300 \
        W20=fec00301 W22=0190 W22=0300 W22=0402 W22=0733 W22=0811 W22=10b0 W22=1100 W22=1380 \
        W22=1500 W22=1701 W22=1c59 W22=1daa W22=1e20 W22=1f19 W22=857a W22=8704 W22=8e26 W22=a391 \
        W22=a540 W22=fec02ab9 W22=fec006101000 W22=fec00a00 W22=fec00e00 W22=fec02ab9 W22=fec02970 \
        W22=fec0283e W22=fec02a19 W22=fec01c00 W22=fec011c0a7 W22=0713";
    // tune_t: the vendor's T wake (fec00b01 fec01201 0300) follows the S park.
    const TUNE_T_HEAD: &str = " W22=1e28 W22=0a00 W22=0301 W22=1701 R22=1e/1 W22=1e38 \
        W22=fec02ab9 W22=fec02970 W22=fec0283e W22=fec02a19 W22=fec01c00 W22=fec011c0a7 W22=0713 \
        W20=fec00b01 W20=fec01201 W20=0300 \
        W20=0300 W20=1daa W20=1eaa W20=1faa W20=2500 W20=234d W20=fec0fb0d R20=fec1/1 W20=fec00d00 \
        W20=fec00f20 W20=fec01049";
    const TUNE_T_TAIL: &str = " W20=fec00001 W20=fec03181 W20=fec00000 W20=fec07c1f W20=fec01200 \
        W20=fec0fb6e R20=fec1/1 W20=fec06e81 W20=fec0fb14 R20=fec1/1 W20=fec01413 W20=fec01201 \
        W20=fec0fb6e R20=fec1/1 W20=fec06e01 R20=0e/1 W20=0e71 W20=0f10 W20=0140 W20=234c R20=80/1 \
        S120 W20=1d02 W20=1e2a W20=1f8a R20=77/1 W20=775a R20=76/1 W20=765a R20=5c/1 W20=5c5a";
    const TUNE_S_1318_40F1: &str = " W20=2500 S3 W20=234d S3 W20=0390 S3 W20=fec01200 S3 \
        W20=fec00b00 S3 R22=1e/1 S3 W22=1e28 S3 W22=fec01100a7 S3 W22=fec02ab9 S80 W22=fec01cc0 \
        S80 W22=fec02971 S80 W22=1700 S3 W22=1e20 S3 W22=0aff S3 W22=0301 S3 W22=1caa S3 W22=1f2a \
        S3 W22=8f40 S3 W22=90f1 S3 W22=1e28 S3 W22=0a00 S3 W22=0301 S3 W22=fec01100a7 S3 \
        W22=fec02ab9 S80 W22=fec01cc0 S80 W22=fec02971 S80 W22=fec0000a4c000000 S80 W22=fec0098c \
        S3 W22=fec037ac S3 W22=fec00580 S3 W22=fec02abb S80 W20=0e77 S3 W20=0f77 S3 R22=07/1 S3 \
        W22=0711 S3 W22=0810 S3 R22=fec1/2 S3 W22=fec050f0 S3 W22=1e20 S3 W22=0aff S3 W22=0301 S3 \
        W22=0301 S3 W22=1c08 S3 W22=1f08 S3 R22=a5/1 S3 W22=a540 S3 R22=a3/1 S3 W22=a380 S3 \
        R22=c3/1";
    const TUNE_S_1613_7040: &str = " W20=2500 S3 W20=234d S3 W20=0390 S3 W20=fec01200 S3 \
        W20=fec00b00 S3 R22=1e/1 S3 W22=1e28 S3 W22=fec01100a7 S3 W22=fec02ab9 S80 W22=fec01cc0 \
        S80 W22=fec02971 S80 W22=1700 S3 W22=1e20 S3 W22=0aff S3 W22=0301 S3 W22=1caa S3 W22=1f2a \
        S3 W22=8f70 S3 W22=9040 S3 W22=1e28 S3 W22=0a00 S3 W22=0301 S3 W22=fec01100a7 S3 \
        W22=fec02ab9 S80 W22=fec01cc0 S80 W22=fec02971 S80 W22=fec0000c9a000000 S80 W22=fec00944 \
        S3 W22=fec037ac S3 W22=fec00580 S3 W22=fec02abb S80 W20=0e77 S3 W20=0f77 S3 R22=07/1 S3 \
        W22=0711 S3 W22=0810 S3 R22=fec1/2 S3 W22=fec050f0 S3 W22=1e20 S3 W22=0aff S3 W22=0301 S3 \
        W22=0301 S3 W22=1c08 S3 W22=1f08 S3 R22=a5/1 S3 W22=a540 S3 R22=a3/1 S3 W22=a380 S3 \
        R22=c3/1";

    #[test]
    fn power_on_sequence() {
        assert_eq!(rec(|b| power_on_init(b).unwrap()), POWER_ON);
    }

    #[test]
    fn tune_t_sequence() {
        let want = |r11: &str| format!("{TUNE_T_HEAD} W20=fec011{r11}{TUNE_T_TAIL}");
        assert_eq!(rec(|b| tune_t(b, 557_142_857).unwrap()), want("8b"));
        assert_eq!(rec(|b| tune_t(b, 473_142_857).unwrap()), want("76"));
        assert_eq!(terr_regs(557_142_857), (0x49, 0x8b));
        // odd channel number -> bit 7 of reg 0x10
        assert_eq!(terr_regs(479_142_857), (0xC9, 0x77));
        // CATV and the VHF range the same way (C13, C22, C63)
        assert_eq!(terr_regs(111_142_857), (0xC9, 0x1B));
        assert_eq!(terr_regs(167_142_857), (0xC9, 0x29));
        assert_eq!(terr_regs(465_142_857), (0x49, 0x74));
        // 700 MHz and up (UHF 51ch, 52ch): two tuner settings change
        let tail = rec(|b| tune_t(b, 701_142_857).unwrap());
        assert!(tail.contains("W20=fec01049 W20=fec011af"), "{tail}");
        assert!(
            tail.contains("W20=fec03191 W20=fec00000 W20=fec07c9f"),
            "{tail}"
        );
        assert_eq!(terr_band_regs(699_999_999), (0x81, 0x1F));
    }

    /// Demod reg 0x80 reads follow `script` (its last value repeats); the I2C log keeps
    /// the writes.
    struct Lock {
        script: Vec<u8>,
        polls: usize,
        writes: String,
    }

    impl Bus for Lock {
        fn i2c_write(&mut self, slave: u8, data: &[u8]) -> Result<()> {
            write!(self.writes, " W{slave:02x}={}", hex(data)).unwrap();
            Ok(())
        }
        fn i2c_write_read(&mut self, slave: u8, w: &[u8], r: &mut [u8]) -> Result<()> {
            r.fill(0x5A);
            if slave == DEMOD_T && w == [0x80] {
                r[0] = self.script[self.polls.min(self.script.len() - 1)];
                self.polls += 1;
            }
            Ok(())
        }
        fn sleep_ms(&mut self, _ms: u64) {}
    }

    /// (lock polls, locked) of `wait_lock_t` on the reg 0x80 values `script`.
    fn wait_t(script: &[u8]) -> (usize, bool) {
        let mut b = Lock {
            script: script.to_vec(),
            polls: 0,
            writes: String::new(),
        };
        wait_lock_t(&mut b);
        (b.polls, b.writes.contains("W20=1d02"))
    }

    #[test]
    fn t_lock_wait_gives_up_on_an_empty_channel() {
        // the hardware's empty channel: 0x2f, then 0xaf from ~0.15 s on
        assert_eq!(wait_t(&[0x2f, 0x2f, 0xaf]), (2 + 16, false));
        // bit 7 must stay set: one clear poll starts the count again
        let mut s = vec![0xaf; 15];
        s.extend([0x2f, 0xaf]);
        assert_eq!(wait_t(&s), (15 + 1 + 16, false));
    }

    #[test]
    fn t_lock_wait_keeps_a_present_signal() {
        // the hardware's present channel: no bit 7, locks after ~1 s
        let s = [
            0x2f, 0x2f, 0x2f, 0x2f, 0x2f, 0x2f, 0x2e, 0x0e, 0x06, 0x06, 0x02, 0x00,
        ];
        assert_eq!(wait_t(&s), (12, true));
        // a signal that does not lock waits the whole ~3.4 s
        assert_eq!(wait_t(&[0x2f, 0x06]), (60, false));
    }

    #[test]
    fn tune_s_sequence() {
        for (hz, tsid, want) in [
            (1_318_000_000, 0x40F1, TUNE_S_1318_40F1),
            (1_613_000_000, 0x7040, TUNE_S_1613_7040),
        ] {
            let got = rec(|b| {
                tune_s(b, hz, tsid).unwrap();
            });
            assert_eq!(&got[..want.len()], want);
            // locked: the TSID streamed (read again while it is not the one asked for;
            // the recorder answers 0), then the transponder's TSIDs
            let tail = &got[want.len()..];
            assert_eq!(tail.matches(" R22=e6/2").count(), 10);
            assert!(tail.ends_with(" R22=ce/16"), "{tail}");
        }
    }

    /// A firmware that stopped acking: every transfer fails; counts the time slept.
    #[derive(Default)]
    struct Wedged(u64);

    impl Bus for Wedged {
        fn i2c_write(&mut self, _: u8, _: &[u8]) -> Result<()> {
            Err(Error::Timeout("wedged"))
        }
        fn i2c_write_read(&mut self, _: u8, _: &[u8], _: &mut [u8]) -> Result<()> {
            Err(Error::Timeout("wedged"))
        }
        fn sleep_ms(&mut self, ms: u64) {
            self.0 += ms;
        }
        fn wedged(&self) -> bool {
            true
        }
    }

    #[test]
    fn a_wedged_firmware_fails_the_tune_without_a_lock_wait() {
        let mut b = Wedged::default();
        assert!(tune_s(&mut b, 1_318_000_000, 0x40F1).is_err());
        assert!(b.0 < 1000, "slept {} ms", b.0); // not the 8 s lock wait
        let mut b = Wedged::default();
        assert!(tune_t(&mut b, 557_142_857).is_err());
        assert!(b.0 < 1000, "slept {} ms", b.0);
    }

    #[test]
    fn sample_reads() {
        let mut r = Rec::default();
        assert_eq!(sample_s(&mut r).unwrap(), (0, 23130, 5_921_370, 5_921_370));
        assert_eq!(r.0, " R22=c3/1 R22=bc/2 R22=eb/3 R22=f0/3");
        let mut r = Rec::default();
        assert_eq!(sample_t(&mut r).unwrap(), (0, 5_921_370));
        assert_eq!(r.0, " R20=80/1 R20=8b/3");
    }

    #[test]
    fn status_reads_real_cn() {
        let mut r = Rec::default();
        let st = read_status(&mut r, System::Satellite).unwrap();
        assert!(st.locked);
        assert_eq!((st.raw, st.cn_mdb), (0x5A5A, cn_s_mdb(0x5A5A)));
        let st = read_status(&mut r, System::Terrestrial).unwrap();
        assert_eq!((st.raw, st.cn_mdb), (0x5A5A5A, cn_t_mdb(0x5A5A5A)));
    }
}
