//! The Fujitsu I2C controller driven from the host through the 0xBC window, at its
//! USB-core-block instance 0x00FD8FE0. This is the SAME controller the vendor's aux core
//! drives at 0x01684FE0 (the 0x0168 demod I2C), but reached from its host face, which
//! answers the host directly -- unlike the aux face, which wedges the main core's shared
//! USB host interface. So the host can read (and tune) the demod blob-free, with no aux
//! core and no wedge. The register map, the setup values and the byte-level handshake
//! are from analysis provided by a third party, who drove this controller from the host
//! on a PIX-DT295.

use dt295_core::proto::Io;
use dt295_core::{Error, Result, Status, System, fe1};

/// The USB-core-block instance base (the host face of the demod I2C controller).
const BASE: u32 = 0x00FD_8FE0;
const O_STATUS: u32 = 0x00; // bit7 busy, bit4 last ACK missing
const O_CONTROL: u32 = 0x04; // bit7 error, bit5 restart, bit4 start, bit3 ack, bit0 done
const O_DIVIDER: u32 = 0x08;
const O_DATA: u32 = 0x10;
const O_SETUP_14: u32 = 0x14; // 0x10 on setup (this instance only)
const O_SETUP_18: u32 = 0x18; // 6 on setup (this instance only)

const CT_INITIAL: u32 = 0x42;
const DIV_SETUP: u32 = 0x3c;
const DIV_MASK: u32 = 0x7f; // bit7 is a status bit, not what we wrote
const ST_BUSY: u32 = 0x80;
const ST_NACK: u32 = 0x10;
const CT_ERROR: u32 = 0x80;
const CT_RESTART: u32 = 0x20;
const CT_START: u32 = 0x10;
const CT_ACK: u32 = 0x08;
const CT_DONE: u32 = 0x01;
/// The in-chip path spins I2C_SPIN (0x20000); from the host each access is a ~60 us
/// control transfer and the bit has always settled, so a small cap is plenty.
const SPIN: u32 = 2000;

fn r(io: &Io, off: u32) -> Result<u32> {
    let mut b = [0u8; 4];
    io.read_block(BASE + off, &mut b)?;
    Ok(u32::from_be_bytes(b))
}
fn w(io: &Io, off: u32, v: u32) -> Result<()> {
    io.write_block(BASE + off, &v.to_be_bytes())
}
fn abs_r(io: &Io, addr: u32) -> Result<u32> {
    let mut b = [0u8; 4];
    io.read_block(addr, &mut b)?;
    Ok(u32::from_be_bytes(b))
}
fn abs_w(io: &Io, addr: u32, v: u32) -> Result<()> {
    io.write_block(addr, &v.to_be_bytes())
}

/// Clock and un-reset the instance (per a third party's analysis it "has to be clocked
/// and un-reset before any write"), then write the divider, control and the two
/// instance-only setup words, and confirm they took. Idempotent.
fn setup(io: &Io) -> Result<()> {
    // The full gate (clock, reset release, 0x80B0, the 0x80C0 reset-hold).
    abs_w(io, 0x00FD_8010, abs_r(io, 0x00FD_8010)? | 0x10)?; // block clock
    abs_w(io, 0x00FD_8014, abs_r(io, 0x00FD_8014)? & !0x10)?; // reset release
    abs_w(io, 0x00FD_80B0, abs_r(io, 0x00FD_80B0)? | 1)?;
    abs_w(io, 0x00FD_80C0, 0)?;
    w(io, O_SETUP_14, 0x10)?;
    w(io, O_SETUP_18, 6)?;
    w(io, O_DIVIDER, DIV_SETUP)?;
    w(io, O_CONTROL, CT_INITIAL)?;
    if r(io, O_CONTROL)? & !CT_DONE != CT_INITIAL {
        return Err(Error::State("fj I2C: control setup did not take".into()));
    }
    if r(io, O_DIVIDER)? & DIV_MASK != DIV_SETUP {
        return Err(Error::State("fj I2C: divider setup did not take".into()));
    }
    Ok(())
}

fn wait_idle(io: &Io) -> Result<()> {
    for _ in 0..SPIN {
        if r(io, O_STATUS)? & ST_BUSY == 0 {
            return Ok(());
        }
    }
    Err(Error::Timeout("fj I2C idle"))
}

/// Read one 8-bit register of the demod (or any slave) over the host face of the I2C,
/// using the corrected handshake (`xfer2`: CONTROL bit4 held through every byte, each
/// byte waiting on CONTROL DONE). `slave` is the 7-bit address.
pub fn demod_read(io: &Io, slave: u8, reg: u8) -> Result<u8> {
    setup(io)?;
    let mut rb = [0u8; 1];
    xfer2(io, slave, &[reg], &mut rb)?;
    Ok(rb[0])
}

// ---------------------------------------------------------------------------
// Experimental: blob-free COLD bring-up of the demod / I2C block from the host.
//
// Our `setup` only clocks the I2C controller instance (0xFD80xx + the controller
// registers). A third-party host-driven driver shows that a cold
// device also needs a board-level clock/power chain (0xFD0090 external memory,
// the INITIALIZE clock/strap/pin writes on 0xF001F0 / 0xF100xx / 0xF101xx, the
// 0xF10114 I2C-block enable) BEFORE the controller will actually clock a byte --
// which is why our cold host-face read never advanced DONE. These are the facts
// from that analysis, reproduced here over the host 0xBC window to verify on our
// own hardware; we must drive them from the host (the main FR swallows 0x00F10xxx
// writes cold). The 0x16xxxxx demod sub-block step of INITIALIZE is above the
// 24-bit 0xBC window and is omitted for now (so this is the host-reachable subset).
// Never write 0xFD8010/0xFD8014 = 0x08080000 (kills all control transfers).

const FD0090: u32 = 0x00FD_0090;
const F10114: u32 = 0x00F1_0114;
const F001F0: u32 = 0x00F0_01F0;
const F10004: u32 = 0x00F1_0004;
const F10014: u32 = 0x00F1_0014;
const F10018: u32 = 0x00F1_0018;
const F1001C: u32 = 0x00F1_001C;
const F10118: u32 = 0x00F1_0118;
const F1011C: u32 = 0x00F1_011C;
const F10080: [u32; 7] = [
    0x00F1_0080, 0x00F1_0084, 0x00F1_0088, 0x00F1_008C, 0x00F1_0090, 0x00F1_0094, 0x00F1_0098,
];

/// A register touched during cold bring-up, with the original value to put back.
/// `half` = a 16-bit register (restored with `write16`), else a 32-bit word.
type Saved = Vec<(u32, bool, u32)>;

fn rd32(io: &Io, addr: u32) -> Result<u32> {
    let mut b = [0u8; 4];
    io.read_block(addr, &mut b)?;
    Ok(u32::from_be_bytes(b))
}

/// Do the host-reachable part of the cold board bring-up, pushing every modified
/// register's original value into `saved` as we go (so the caller can always put
/// them back, even on a mid-sequence error). Appends a running diagnostic to `log`.
fn cold_bringup_host(io: &Io, saved: &mut Saved, log: &mut Vec<String>) -> Result<()> {
    // External memory (0xFD0090, 16-bit): set bit1, wait bit4 clear, clear bit1, set bit0.
    io.write16(FD0090, io.read16(FD0090)? | 0x0002)?;
    let mut polls = 0u32;
    let mut cleared = false;
    while polls < 4000 {
        if io.read16(FD0090)? & 0x0010 == 0 {
            cleared = true;
            break;
        }
        polls += 1;
    }
    io.write16(FD0090, io.read16(FD0090)? & !0x0002)?;
    io.write16(FD0090, io.read16(FD0090)? | 0x0001)?;
    log.push(format!(
        "0xFD0090 ext-mem: bit4 clear after {polls} polls ({})",
        if cleared { "ok" } else { "TIMEOUT" }
    ));

    // 0xF10080..0x98 = 0x00010000 (write only -- reading these wedges the chip).
    for a in F10080 {
        io.write_block(a, &0x0001_0000u32.to_be_bytes())?;
        saved.push((a, false, 0)); // original is treated as 0 on restore
    }

    // INITIALIZE: the vendor's clock / strap / pin bring-up (spec ch6 in the third-party
    // host driver), minus the 0x16xxxxx demod sub-block (done later through the bank
    // window). The register ROLES below are what a third party documented; the exact bit
    // meanings are mostly not established, so the literal values are the observed vendor
    // ones. Originals are saved for the restore.

    // Steps 1-3: clock / pin config. 0xF10118 = 0xFFFF; 0xF1011C keeps bits 0xFA1F, sets 0x0580.
    let o = io.read16(F10118)?;
    saved.push((F10118, true, u32::from(o)));
    io.write16(F10118, 0xFFFF)?;
    let o = io.read16(F1011C)?;
    saved.push((F1011C, true, u32::from(o)));
    io.write16(F1011C, (o & 0xFA1F) | 0x0580)?;
    log.push("0x16xxxxx sub-block step skipped (above 24-bit window)".into());

    // Steps 7-9: the host-interface control register 0xF001F0. Four bits are selected from
    // the board strap in 0xF10004 bits 11:10 (DT295 reads strap 2): bit28/27/26 from the
    // strap, then bit25 unconditionally. Observed on DT295 as 0x10000000 -> 0x18000000 ->
    // 0x1A000000, so bit25 (0x02000000) is the last to go in.
    let f001f0 = rd32(io, F001F0)?;
    saved.push((F001F0, false, f001f0));
    let f10004 = io.read16(F10004)?;
    let mut f14 = io.read16(F10014)?;
    saved.push((F10014, true, u32::from(f14)));
    let mut f18 = io.read16(F10018)?;
    saved.push((F10018, true, u32::from(f18)));
    if f001f0 & 0x4000 != 0 {
        // The vendor sequence only documents the strap branch with bit14 clear; bail rather
        // than guess the bit layout of the other (bit14 set) branch.
        log.push(format!(
            "0xF001F0=0x{f001f0:08x} bit14 set -- expected the bit14=0 strap branch; aborting"
        ));
        return Err(Error::State("F001F0 bit14 set (unexpected strap)".into()));
    }
    let strap = f001f0_strap(f10004) & 3;
    let (a, b, c) = (strap != 0, strap == 1, strap == 2);
    log.push(format!(
        "strap (0xF10004>>10 & 3) = {strap} (DT295 expects 2): a={a} b={b} c={c}"
    ));
    let mut v = f001f0;
    for (bit, on) in [(28u32, a), (27, c), (26, b), (25, true)] {
        v = if on { v | (1 << bit) } else { v & !(1 << bit) };
        io.write_block(F001F0, &v.to_be_bytes())?;
        let back = rd32(io, F001F0)?; // each bit is written one at a time and read back
        if back != v {
            log.push(format!(
                "0xF001F0 bit{bit}={on}: wrote 0x{v:08x} read 0x{back:08x} MISMATCH"
            ));
        }
    }
    log.push(format!("0xF001F0 -> 0x{:08x}", rd32(io, F001F0)?));

    // Steps 10-12: the clock / reset gates 0xF10014 and 0xF10018. The strap-a branch toggles
    // the 0x80 pair first; then 0xF10014 clears 0x200, sets 0xD000, and finally 0xF10014 |= 2
    // with 0xF10018 &= ~2 (the vendor writes these two together and verifies both).
    if a {
        f14 &= 0xFF9F;
        io.write16(F10014, f14)?;
        f18 &= !0x80;
        io.write16(F10018, f18)?;
        f14 |= 0x80;
        io.write16(F10014, f14)?;
    }
    f14 &= !0x200;
    io.write16(F10014, f14)?;
    f14 |= 0xD000;
    io.write16(F10014, f14)?;
    f14 |= 2;
    f18 &= !2;
    io.write16(F10014, f14)?;
    io.write16(F10018, f18)?;

    // 0xF1001C: a clock / reset gate (the vendor also drives it for the I2C block); drop 0x5080.
    let o = io.read16(F1001C)?;
    saved.push((F1001C, true, u32::from(o)));
    io.write16(F1001C, o & !0x5080)?;

    // Post-INITIALIZE enables the vendor's INITIALIZE SERVICE does: 0xF10018 = 0x0002 and
    // 0xF1001C |= 0x0002.
    io.write16(F10018, 0x0002)?;
    io.write16(F1001C, io.read16(F1001C)? | 0x0002)?;

    // I2C-block power: 0xF10114 |= bit21 (the vendor's I2C-block SERVICE takes it 0x80000000
    // -> 0x80200000). This host 0xBC write landing on a cold chip is the crux of the whole
    // approach ("A0"): if it is swallowed, nothing downstream works.
    let before = rd32(io, F10114)?;
    saved.push((F10114, false, before));
    io.write_block(F10114, &(before | 0x0020_0000).to_be_bytes())?;
    let after = rd32(io, F10114)?;
    log.push(format!(
        "0xF10114 (I2C block): before=0x{before:08x} after=0x{after:08x} -> {}",
        if after & 0x0020_0000 != 0 {
            "bit21 SET (host 0xBC write landed cold)"
        } else {
            "bit21 NOT SET -- host 0xBC write swallowed cold"
        }
    ));
    Ok(())
}

fn f001f0_strap(f10004: u16) -> u32 {
    u32::from(f10004) >> 10
}

// --- Host 0xBC access to 25-bit addresses via the stock boot0 bank window ---
// A third-party host driver reaches 0x1000000+ (the demod 0x0160xxxx and capture
// 0x0190xxxx blocks) NOT through an on-chip monitor but by the stock boot0's own bank
// window: write the target bank into the boot0 LDI:32 immediate (write side 0xEA3D8, read
// side 0xE9EBC) and access alias 0x80000 + (addr & 0x1FFFF). Every access stays inside the
// 24-bit 0xBC window, and it writes the hardware registers directly (no CPU-cache step the
// monitor's on-chip stores can get stuck in -- which is why a monitor-configured cold
// capture engine ran but read an idle input).
const WRITE_WINDOW: u32 = 0x000E_A3D8;
const READ_WINDOW: u32 = 0x000E_9EBC;
const ALIAS_BASE: u32 = 0x0008_0000;
const BANK_MASK: u32 = 0x0001_FFFF;
const DIRECT_END: u32 = 0x0100_0000;

fn window_set(io: &Io, window: u32, bank: u32) -> Result<()> {
    io.write_block(window, &bank.to_be_bytes())?;
    if rd32(io, window)? != bank {
        return Err(Error::State(format!("window 0x{window:x} would not hold 0x{bank:08x}")));
    }
    Ok(())
}

fn win_write32(io: &Io, addr: u32, val: u32) -> Result<()> {
    if addr < DIRECT_END {
        return io.write_block(addr, &val.to_be_bytes());
    }
    window_set(io, WRITE_WINDOW, addr & !BANK_MASK)?;
    io.write_block(ALIAS_BASE + (addr & BANK_MASK), &val.to_be_bytes())
}

fn win_write16(io: &Io, addr: u32, val: u16) -> Result<()> {
    if addr < DIRECT_END {
        return io.write16(addr, val);
    }
    window_set(io, WRITE_WINDOW, addr & !BANK_MASK)?;
    io.write16(ALIAS_BASE + (addr & BANK_MASK), val)
}

fn win_read32(io: &Io, addr: u32) -> Result<u32> {
    if addr < DIRECT_END {
        return rd32(io, addr);
    }
    window_set(io, READ_WINDOW, addr & !BANK_MASK)?;
    rd32(io, ALIAS_BASE + (addr & BANK_MASK))
}

/// Restore both bank-window immediates to their default (0x00F00000), so later 0x80000..
/// 0x9FFFF accesses (e.g. the boot-state register 0x82008) map where they should.
fn restore_windows(io: &Io) {
    let _ = window_set(io, WRITE_WINDOW, 0x00F0_0000);
    let _ = window_set(io, READ_WINDOW, 0x00F0_0000);
}

/// The demod sub-block stop of INITIALIZE (spec ch6), on 0x0160xxxx. Done through the
/// stock bank window (host 0xBC), writing the hardware directly. Without it the capture
/// engine free-runs on an idle input (the 0xF0 pattern) instead of the demod TS.
fn init_demod_subblock(io: &Io, log: &mut Vec<String>) -> Result<()> {
    let r0 = win_read32(io, 0x0160_0000)?;
    let r4 = win_read32(io, 0x0160_0004)?;
    let r404 = win_read32(io, 0x0160_0404)?;
    log.push(format!(
        "0x1600000=0x{r0:08x} (exp 0x000200FF) 0x1600004=0x{r4:08x} (exp 0x01000000) 0x1600404=0x{r404:08x}"
    ));
    win_write32(io, 0x0160_0404, r404 | 0x0004_0000)?;
    win_write32(io, 0x0160_0004, 0xFFFF_FFFF)?;
    win_write32(io, 0x0160_0000, 0xFFFF_FFFF)?;
    win_write32(io, 0x0160_0004, 0x0080_0000)?;
    let c4 = win_read32(io, 0x0160_0004)?;
    let c0 = win_read32(io, 0x0160_0000)?;
    log.push(format!(
        "sub-block stopped: 0x1600004=0x{c4:08x} (exp 0x00800000) 0x1600000=0x{c0:08x} (bit17 {})",
        if c0 & 0x2_0000 != 0 { "set" } else { "CLEAR" }
    ));
    Ok(())
}

/// The TS capture ring (byte offsets); the physical address the DMA writes and the length.
const CAP_RING_PHYS: u32 = 0x0138_EC00;
const CAP_RING_LEN: u32 = 0x0003_0000;

/// Capture configure table (addr, value, is_half), applied after the preamble -- a
/// third-party host driver's ConfigureCapture entries, byte for byte. 0x1901xxx is the TS
/// input / sync block, 0x1902xxx the ring DMA. Most values are the observed vendor ones;
/// the few whose role is known are noted.
const CAPTURE_TABLE: &[(u32, u32, bool)] = &[
    (0x0190_1040, 0, false),
    (0x0190_1064, 0, false),
    (0x0190_10A0, 0, false),
    (0x0190_10A4, 0, false),
    (0x0190_10A8, 0, false),
    (0x0190_1000, 0x0600, true),
    (0x0190_1020, 0x0200_0000, false),
    (0x0190_1004, 0x0400_0000, false),
    (0x0190_1002, 0, true),
    (0x0190_1044, 0x00BC_0000, false), // TS packet geometry (0xBC = 188 bytes)
    (0x0190_1048, 0x4700_0000, false), // the TS sync byte (0x47) the input aligns on
    (0x0190_104C, 0x00FF_FFFF, false),
    (0x0190_1050, 0, false),
    (0x0190_1060, 0, false),
    (0x0190_1080, 0x0100_0000, false),
    (0x0190_1088, 0, false),
    (0x0190_108C, 0xFFFF_FFFF, false),
    (0x0190_1084, 0, false),
    (0x0190_1090, 0x0100_0000, false),
    (0x0190_2020, CAP_RING_PHYS, false), // ring base (physical)
    (0x0190_2024, CAP_RING_LEN, false),  // ring length
    (0x0190_2028, 0, false),             // ring write pointer (hardware advances it)
    (0x0190_202C, 0, false),             // ring read pointer (the pump advances it)
    (0x0190_2030, 0, false),
    (0x0190_2000, 0x0302_0300, false), // capture format / mode (observed)
    (0x00F1_2788, 0, true),            // TS input gate
];

/// Configure the capture engine over the host 0xBC bank window (a third-party host
/// driver's ConfigureCapture). Writes the hardware registers directly, NOT through the
/// monitor. Does not start the engine (that is [`host_start_capture`], after the endpoint).
fn host_configure_capture(io: &Io, log: &mut Vec<String>) -> Result<()> {
    if win_read32(io, 0x0190_2010)? != 0 {
        log.push("warn: 0x1902010 != 0 before configure".into());
    }
    // Preamble (values observed from the vendor's ConfigureCapture): 0x1900404 |= 0x40000
    // enables the block; 0x1900000 = 0x3002FFFF then 0x00020000 is a reset-then-config pulse
    // (so 0x1900020 below MUST be written after it, or the reset clears it); 0x1900004 is set
    // around it; 0x1900020 = 0x22000200 is the bit29 a cold arm needs.
    let r404 = win_read32(io, 0x0190_0404)?;
    win_write32(io, 0x0190_0404, r404 | 0x0004_0000)?;
    win_write32(io, 0x0190_0004, 0x001F_11FF)?;
    win_write32(io, 0x0190_0000, 0x3002_FFFF)?;
    win_write32(io, 0x0190_0000, 0x0002_0000)?;
    win_write32(io, 0x0190_0004, 0x0000_1100)?;
    win_write32(io, 0x0190_0020, 0x2200_0200)?;
    // The TS-input / sync + ring-DMA table.
    for &(addr, val, half) in CAPTURE_TABLE {
        if half {
            win_write16(io, addr, val as u16)?;
        } else {
            win_write32(io, addr, val)?;
        }
    }
    // Postamble (observed): arm the input (0x1901000 = 0x0607), clear then set the DMA
    // control 0x1901040 (0 -> 0x03000000), and 0x19010A4 = 0x13170400.
    win_write16(io, 0x0190_1000, 0x0607)?;
    win_write32(io, 0x0190_1040, 0)?;
    win_write32(io, 0x0190_1020, 0)?;
    win_write32(io, 0x0190_1040, 0x0300_0000)?;
    win_write32(io, 0x0190_10A4, 0x1317_0400)?;
    log.push("capture configured over the host bank window".into());
    Ok(())
}

/// Start the capture engine: 0x1902010 = 0x02000000, over the host bank window.
fn host_start_capture(io: &Io) -> Result<()> {
    win_write32(io, 0x0190_2010, 0x0200_0000)
}

/// The capture/endpoint protection register: write the target group before a guarded
/// register, 0xF after.
const PROTECT: u32 = 0x00FD_814C;

fn guarded(io: &Io, group: u32, addr: u32, val: u32) -> Result<()> {
    io.write_block(PROTECT, &group.to_be_bytes())?;
    io.write_block(addr, &val.to_be_bytes())?;
    io.write_block(PROTECT, &0x0000_000Fu32.to_be_bytes())
}

/// SetUpUsb (the safe subset): enable the bulk-IN TS endpoint in the USB core. Done early,
/// during board bring-up, like a third-party host driver. Host 0xBC on 0xFDBCxx; never
/// writes 0xFD8010 / 0xFD8014 = 0x08080000 (that kills every control transfer).
fn usb_core_enable(io: &Io, log: &mut Vec<String>) -> Result<()> {
    guarded(io, 1, 0x00FD_BC04, 0x8500_0000)?;
    for v in [0x04u32, 0x84] {
        guarded(io, 8, 0x00FD_BC14, v)?;
        guarded(io, 2, 0x00FD_BC14, v)?;
    }
    log.push("USB core bulk-IN endpoint enabled".into());
    Ok(())
}

/// StartEndpoint: reset the TS endpoint, set its ring buffer address, enable it. A
/// third-party host driver does this AFTER configuring the capture engine (the capture
/// block reset in CONFIGURE would otherwise disturb it), so the cold arm calls it between
/// the capture configure pass and the capture start.
fn start_endpoint(io: &Io, log: &mut Vec<String>) -> Result<()> {
    let saved14 = rd32(io, 0x00FD_BC14)?;
    guarded(io, 8, 0x00FD_BD08, 1)?;
    io.write_block(PROTECT, &0x0000_0006u32.to_be_bytes())?;
    io.write_block(0x00FD_BD08, &0x00FF_6000u32.to_be_bytes())?;
    let buffer = rd32(io, 0x00FD_BD08)?;
    io.write_block(0x00FD_BD08, &buffer.to_be_bytes())?;
    io.write_block(PROTECT, &0x0000_000Fu32.to_be_bytes())?;
    let enable = saved14 & 0xFFFD_FFFD;
    guarded(io, 2, 0x00FD_BC14, enable)?;
    guarded(io, 8, 0x00FD_BC14, enable)?;
    log.push(format!(
        "TS endpoint started (0xFDBC14=0x{enable:08x}, buffer=0x{buffer:08x})"
    ));
    Ok(())
}

/// Put the saved registers back, in reverse order.
fn cold_restore(io: &Io, saved: &Saved) {
    for &(addr, half, val) in saved.iter().rev() {
        let _ = if half {
            io.write16(addr, val as u16)
        } else {
            io.write_block(addr, &val.to_be_bytes())
        };
    }
}

/// Wait for the current I2C byte to finish by polling CONTROL DONE (bit0), the way a
/// register-level driver does. Returns (done, last CONTROL value). The caller carries
/// the hardware CONTROL forward -- only toggling DONE / START / RESTART / ACK as the
/// protocol needs -- so per-byte state bits the controller set are not dropped (that
/// was why only the first, START-carrying byte clocked). Fails on a bus error.
fn wait_done(io: &Io) -> Result<(bool, u32)> {
    let mut last = r(io, O_CONTROL)?;
    for _ in 0..SPIN {
        let v = r(io, O_CONTROL)?;
        last = v;
        if v & CT_ERROR != 0 {
            return Err(Error::State(format!("fj I2C bus error (CONTROL=0x{v:04x})")));
        }
        if v & CT_DONE != 0 {
            return Ok((true, v));
        }
    }
    Ok((false, last))
}

/// A byte that never reached DONE within [`SPIN`] polls is a stuck or dead bus; fail rather
/// than read a stale DATA byte as if the transfer had completed.
fn require_done(done: bool) -> Result<()> {
    done.ok_or(Error::Timeout("fj I2C: byte did not complete"))
}

/// One cold register read, carrying the hardware CONTROL forward per byte (a third-party
/// host driver's model), with DONE diagnostics at every step.
fn cold_xfer_read(io: &Io, slave: u8, reg: u8, log: &mut Vec<String>) -> Result<u8> {
    let addr7 = u32::from(slave) & 0x7f;
    wait_idle(io)?;
    // address byte, write direction, with START.
    w(io, O_DATA, addr7 << 1)?;
    w(io, O_CONTROL, CT_INITIAL | CT_START)?;
    let (d, mut ctl) = wait_done(io)?;
    let st = r(io, O_STATUS)?;
    log.push(format!(
        "addr 0x{slave:02x} W: DONE={d} CONTROL=0x{ctl:04x} STATUS=0x{st:04x} ACK={}",
        if st & ST_NACK == 0 { "yes" } else { "NO" }
    ));
    if st & ST_NACK != 0 {
        let _ = w(io, O_CONTROL, ctl & !(CT_DONE | CT_ACK | CT_START | CT_RESTART));
        return Err(Error::State(format!("slave 0x{slave:02x} did not ACK")));
    }
    // register byte: advance by clearing only DONE. CONTROL bit4 ("start") is the
    // bus-active bit here, held set through every byte and dropped only at STOP (it
    // read back 0x53 above), so clearing it mid-transfer stalled the controller.
    w(io, O_DATA, u32::from(reg))?;
    w(io, O_CONTROL, ctl & !CT_DONE)?;
    let (d, c) = wait_done(io)?;
    ctl = c;
    log.push(format!("reg 0x{reg:02x} W: DONE={d} CONTROL=0x{ctl:04x}"));
    // turn the bus around with a RESTART, re-send the address as a read.
    w(io, O_DATA, (addr7 << 1) | 1)?;
    w(io, O_CONTROL, (ctl & !CT_DONE) | CT_RESTART)?;
    let (d, c) = wait_done(io)?;
    ctl = c;
    let st = r(io, O_STATUS)?;
    log.push(format!(
        "restart R 0x{slave:02x}: DONE={d} CONTROL=0x{ctl:04x} ACK={}",
        if st & ST_NACK == 0 { "yes" } else { "NO" }
    ));
    // read one byte and NACK it (single-byte read): clear ACK, DONE, RESTART.
    w(io, O_CONTROL, ctl & !(CT_DONE | CT_ACK | CT_RESTART))?;
    let (d, c) = wait_done(io)?;
    ctl = c;
    let val = (r(io, O_DATA)? & 0xff) as u8;
    log.push(format!("read byte: DONE={d} CONTROL=0x{ctl:04x} DATA=0x{val:02x}"));
    // STOP: bits 0,3,4,5 down together.
    let _ = w(io, O_CONTROL, ctl & !(CT_DONE | CT_ACK | CT_START | CT_RESTART));
    Ok(val)
}

/// Experimental blob-free cold read: do the host board bring-up, set up the I2C
/// controller, and read one demod register with full diagnostics. `restore` puts the
/// board registers back afterwards (cheap re-runs without a re-plug). Returns the
/// register value and the step-by-step report. The demod is 7-bit slave 0x10.
pub fn demod_read_cold(io: &Io, slave: u8, reg: u8, restore: bool) -> (Result<u8>, Vec<String>) {
    let mut log = Vec::new();
    let mut saved: Saved = Vec::new();
    let result = (|| {
        cold_bringup_host(io, &mut saved, &mut log)?;
        setup(io)?;
        log.push(format!(
            "I2C setup: CONTROL=0x{:04x} DIVIDER=0x{:04x} STATUS=0x{:04x}",
            r(io, O_CONTROL)?,
            r(io, O_DIVIDER)?,
            r(io, O_STATUS)?
        ));
        cold_xfer_read(io, slave, reg, &mut log)
    })();
    if restore {
        cold_restore(io, &saved);
        log.push(format!("restored {} board registers", saved.len()));
    }
    (result, log)
}

/// Shared blob-free COLD arm, all host-side. With `full` (the first tune after a cold
/// re-plug / open) it does the whole bring-up: install the monitor (which runs our on-chip
/// TS pump), bring up the board over the 0xBC window, stop the demod sub-block, enable the
/// USB core and run the demod's one-time INITIALIZE. On a later channel change (`full` =
/// false) all of that is already up on the chip, so it is skipped and only the retune +
/// capture rearm run (~1 s instead of ~2.5 s). Always: tune and lock over the host-face
/// I2C, configure the capture engine over the stock bank window (DIRECT to the hardware
/// registers -- not the monitor, whose on-chip stores can sit in CPU cache and never reach
/// it), bring up the TS endpoint, start the engine. Returns whether the demod locked.
fn cold_arm(
    io: &Io,
    terrestrial: bool,
    freq_hz: u32,
    tsid: Option<u16>,
    full: bool,
    saved: &mut Saved,
    log: &mut Vec<String>,
) -> Result<bool> {
    use crate::frmon_host;
    use fe1::Bus as _;

    // Gate first (full path only), read-only, BEFORE any board write: a chip that is not
    // pristine stock boot0 (e.g. vendor firmware running) is refused here without the board
    // bring-up touching it. If our monitor is already installed -- a CLI cold-* command re-run
    // without a re-plug -- downgrade to a retune instead of a second full bring-up, which would
    // fault the already-up chip. The monitor itself is written later (after power_on_init); it
    // coexists with host-face 0xBC access and the bank window (only the staging dispatch), so
    // the capture engine is still configured directly.
    let full = if full {
        !frmon_host::install_check(io)?
    } else {
        false
    };
    if full {
        cold_bringup_host(io, saved, log)?;
        init_demod_subblock(io, log)?;
    }
    setup(io)?; // clock/ready the host-face I2C controller (idempotent; cheap on a retune)
    if full {
        usb_core_enable(io, log)?;
        // Power the front end up, then write the monitor: after the board is up but before the
        // tune and capture. Its BODY is the "brought up" marker (`monitor_installed`), so a
        // tune/capture failure below keeps it and the next tune only retunes, while a failure
        // in the bring-up above leaves it absent and the next tune redoes the bring-up (never a
        // second full bring-up of an already-up chip, which faults).
        {
            let mut bus = FjBus::new(io);
            fe1::power_on_init(&mut bus)?;
        }
        frmon_host::install_write(io)?;
    }
    let locked = {
        let mut bus = FjBus::new(io);
        let sys = if terrestrial {
            // The demod INITIALIZE close the vendor front end relies on (0x47/0x4F/0x72): in
            // the vendor-bootstrapped path the resident wrote these, so fe1 omits them; cold
            // there is no resident, and without 0x4F=0x05 the T demod does not drive valid TS
            // onto the capture bus (the engine then reads an idle 0xF0 pattern). 0x70 = 0x18 is
            // a third-party host driver's captured value (fe1's other capture has 0x38; bit5
            // differs). Written before every terrestrial tune, not just the first bring-up, so a
            // bring-up that first locked a BS/CS channel still gets it on the switch to terrestrial.
            bus.i2c_write(0x20, &[0x70, 0x18])?;
            for rv in [[0x47u8, 0x00u8], [0x4F, 0x05], [0x72, 0x00]] {
                bus.i2c_write(0x20, &rv)?;
            }
            fe1::tune_t(&mut bus, freq_hz)?;
            System::Terrestrial
        } else {
            let _ = fe1::tune_s(&mut bus, freq_hz, tsid.unwrap_or(dt295_core::chan::NO_TSID))?;
            System::Satellite
        };
        let st = fe1::read_status(&mut bus, sys)?;
        log.push(format!(
            "tune: locked={} C/N={}.{:03} dB",
            st.locked,
            st.cn_mdb / 1000,
            (st.cn_mdb % 1000).abs()
        ));
        st.locked
    };
    // A third-party host driver's arm order, all host-side over the bank window: configure
    // the capture engine, bring the TS endpoint up, then start the engine last.
    host_configure_capture(io, log)?;
    start_endpoint(io, log)?;
    host_start_capture(io)?;
    log.push("capture started (0x1902010 = 0x02000000, host window)".into());
    Ok(locked)
}

/// Best TS sync over the two record sizes we might see: 188 (bare TS) and 192 (our ring
/// records: 188 TS bytes + a 4-byte suffix). Returns (fraction of records whose first byte
/// is 0x47, record size, phase).
fn ts_sync(buf: &[u8]) -> (f32, usize, usize) {
    let mut best = (0.0f32, 188usize, 0usize);
    for &spacing in &[188usize, 192] {
        if buf.len() < spacing * 2 {
            continue;
        }
        for phase in 0..spacing {
            let (mut hit, mut total) = (0usize, 0usize);
            let mut i = phase;
            while i < buf.len() {
                if buf[i] == 0x47 {
                    hit += 1;
                }
                i += spacing;
                total += 1;
            }
            let frac = hit as f32 / total.max(1) as f32;
            if frac > best.0 {
                best = (frac, spacing, phase);
            }
        }
    }
    best
}

/// Reframe 192-byte ring records (188 TS + 4-byte suffix) at `phase` to bare 188-byte TS.
fn reframe_188(buf: &[u8], phase: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = phase;
    while i + 188 <= buf.len() {
        out.extend_from_slice(&buf[i..i + 188]);
        i += 192;
    }
    out
}

/// Experimental blob-free COLD capture arm (wall C): `cold_arm`, then check the TS
/// ring's write pointer advances -- TS flowing from the demod into the capture ring with
/// no resident, no aux core, no vendor firmware. Leaves chip state up (re-plug to reset).
pub fn cold_capture(
    io: &Io,
    terrestrial: bool,
    freq_hz: u32,
    tsid: Option<u16>,
) -> (Result<bool>, Vec<String>) {
    const RING_WRITE: u32 = 0x0190_2028;
    let mut log = Vec::new();
    let mut saved: Saved = Vec::new();
    let result = (|| -> Result<bool> {
        let locked = cold_arm(io, terrestrial, freq_hz, tsid, true, &mut saved, &mut log)?;
        let rp0 = win_read32(io, RING_WRITE)?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        let rp1 = win_read32(io, RING_WRITE)?;
        let flowing = rp1 != rp0;
        log.push(format!(
            "ring write ptr 0x1902028: 0x{rp0:08x} -> 0x{rp1:08x} => {}",
            if flowing {
                "ADVANCING (TS flowing into the ring)"
            } else {
                "static (no TS)"
            }
        ));
        Ok(locked && flowing)
    })();
    restore_windows(io);
    (result, log)
}

/// Experimental blob-free COLD stream (end to end): `cold_arm`, upload our TS pump, then
/// loop CALLing the pump and reading EP 0x81 for `secs`, counting bytes and TS sync. The
/// whole datapath -- demod power, tune, capture arm, ring->EP pump -- runs with no
/// resident, no aux core and no vendor firmware. Returns (bytes, sync fraction).
pub fn cold_stream(
    io: &Io,
    terrestrial: bool,
    freq_hz: u32,
    tsid: Option<u16>,
    secs: u64,
    out_path: Option<&str>,
) -> (Result<(u64, f32)>, Vec<String>) {
    use crate::frmon_host::MonitorResident;
    use dt295_core::EP_TS;
    use std::time::{Duration, Instant};

    let mut log = Vec::new();
    let mut saved: Saved = Vec::new();
    let result = (|| -> Result<(u64, f32)> {
        cold_arm(io, terrestrial, freq_hz, tsid, true, &mut saved, &mut log)?;
        crate::frmon_host::upload_ts_pump(io)?;
        log.push("TS pump uploaded".into());
        // Keep many reads posted on EP 0x81 the whole time: our pump only commits a FIFO
        // burst when the host has a read pending to drain it, so single reads after each
        // pump never find the FIFO ready. A read queue keeps ~48 reads outstanding.
        let _ = io.usb.abort_pipe(EP_TS);
        let mut queue = io.usb.read_queue(EP_TS, 48, 0x10000).ok();
        if queue.is_none() {
            log.push("WARN: EP 0x81 read queue unavailable; using single reads".into());
        }
        let mut rt = MonitorResident::new(io);
        let mut fallback = vec![0u8; 0x10000];
        let mut saved_bytes: Vec<u8> = Vec::new();
        let mut total: u64 = 0;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(secs) {
            let _ = rt.pump();
            match queue.as_mut() {
                Some(q) => {
                    if let Ok(Some(data)) = q.next(Duration::from_millis(1)) {
                        total += data.len() as u64;
                        if saved_bytes.len() < 4_000_000 {
                            saved_bytes.extend_from_slice(data);
                        }
                    }
                }
                None => {
                    if let Ok(n) = io.usb.read(EP_TS, &mut fallback, Duration::from_millis(20))
                        && n > 0
                    {
                        total += n as u64;
                        if saved_bytes.len() < 4_000_000 {
                            saved_bytes.extend_from_slice(&fallback[..n]);
                        }
                    }
                }
            }
        }
        let secs_f = start.elapsed().as_secs_f32().max(0.001);
        let mbps = (total as f32 * 8.0) / secs_f / 1e6;
        let (sync, spacing, phase) = ts_sync(&saved_bytes);
        log.push(format!(
            "captured {total} bytes in {secs_f:.1}s = {mbps:.1} Mbps; TS sync {:.1}% at {spacing}-byte records (phase {phase})",
            sync * 100.0
        ));
        if let Some(path) = out_path {
            // Write bare 188-byte TS so the file plays / inspects as a transport stream.
            let data = if spacing == 192 && sync > 0.5 {
                reframe_188(&saved_bytes, phase)
            } else {
                saved_bytes.clone()
            };
            std::fs::write(path, &data).map_err(|e| Error::State(format!("write {path}: {e}")))?;
            log.push(format!("wrote {} bytes to {path}", data.len()));
        }
        Ok((total, sync))
    })();
    restore_windows(io);
    (result, log)
}

/// Blob-free COLD bring-up + arm + TS-pump upload, for the BonDriver cold raw-TS worker.
/// `full` does the one-time bring-up (the first tune after a cold open); a channel change
/// passes `full` = false and only retunes + rearms. After this returns, the capture engine
/// streams into the ring and the monitor's TS pump is uploaded; the caller loops
/// [`crate::frmon_host::MonitorResident::pump`] + EP 0x81 reads. Returns whether the demod
/// locked. No resident, no aux core, no vendor firmware.
pub fn cold_prepare_stream(
    io: &Io,
    terrestrial: bool,
    freq_hz: u32,
    tsid: Option<u16>,
    full: bool,
) -> Result<bool> {
    let mut log = Vec::new();
    let mut saved: Saved = Vec::new();
    let armed = cold_arm(io, terrestrial, freq_hz, tsid, full, &mut saved, &mut log);
    for line in &log {
        tracing::info!("cold raw-TS: {line}");
    }
    // Always put the bank window back to its default, even if cold_arm failed after moving it
    // (capture configure / demod sub-block): the worker's pump streams over the staging
    // dispatch, not the window, and a stale capture-bank immediate would mis-map a later direct
    // 0x80000..0x9FFFF access (e.g. the boot-state register 0x82008, the System1 start's reads).
    // cold_capture / cold_stream restore it the same way on every path.
    restore_windows(io);
    let locked = armed?;
    crate::frmon_host::upload_ts_pump(io)?;
    Ok(locked)
}

/// Read the demod's C/N (0.001 dB) over the host face, for a live level during cold
/// streaming. The C/N alone, one I2C transfer: the stream loop cannot pump the TS while it
/// reads (about 5-10 ms a transfer), and the lock is not needed there.
pub fn cold_cn(io: &Io, terrestrial: bool) -> Result<i32> {
    let mut bus = FjBus::new(io);
    let sys = if terrestrial {
        System::Terrestrial
    } else {
        System::Satellite
    };
    fe1::read_cn(&mut bus, sys).map(|(_, cn_mdb)| cn_mdb)
}

/// A general host-face I2C transfer (write `wb`, then read `rb`) with the corrected
/// handshake: CONTROL bit4 is held set through every byte and only dropped at STOP, and
/// each byte waits on CONTROL DONE. `addr7` is the 7-bit slave address.
fn xfer2(io: &Io, addr7: u8, wb: &[u8], rb: &mut [u8]) -> Result<()> {
    let a = (u32::from(addr7) & 0x7f) << 1;
    let read_only = wb.is_empty();
    wait_idle(io)?;
    // address byte (+ read bit if there is nothing to write first), with START.
    w(io, O_DATA, a | u32::from(read_only))?;
    w(io, O_CONTROL, CT_INITIAL | CT_START)?;
    let (done, mut ctl) = wait_done(io)?;
    require_done(done)?;
    if r(io, O_STATUS)? & ST_NACK != 0 {
        let _ = w(io, O_CONTROL, ctl & !(CT_DONE | CT_ACK | CT_START | CT_RESTART));
        return Err(Error::State(format!("fj I2C: slave 0x{addr7:02x} did not ACK")));
    }
    for &byte in wb {
        w(io, O_DATA, u32::from(byte))?;
        w(io, O_CONTROL, ctl & !CT_DONE)?;
        let (done, c) = wait_done(io)?;
        require_done(done)?;
        ctl = c;
    }
    if !rb.is_empty() {
        if !read_only {
            // turn the bus around: RESTART, re-send the address as a read.
            w(io, O_DATA, a | 1)?;
            w(io, O_CONTROL, (ctl & !CT_DONE) | CT_RESTART)?;
            let (done, c) = wait_done(io)?;
            require_done(done)?;
            ctl = c;
            if r(io, O_STATUS)? & ST_NACK != 0 {
                let _ = w(io, O_CONTROL, ctl & !(CT_DONE | CT_ACK | CT_START | CT_RESTART));
                return Err(Error::State(format!("fj I2C: slave 0x{addr7:02x} read NACK")));
            }
        }
        let n = rb.len();
        for (i, slot) in rb.iter_mut().enumerate() {
            let base = ctl & !(CT_DONE | CT_RESTART);
            // ACK every byte but the last (NACK the last to end the read).
            w(io, O_CONTROL, if i + 1 < n { base | CT_ACK } else { base & !CT_ACK })?;
            let (done, c) = wait_done(io)?;
            require_done(done)?;
            ctl = c;
            *slot = (r(io, O_DATA)? & 0xff) as u8;
        }
    }
    let _ = w(io, O_CONTROL, ctl & !(CT_DONE | CT_ACK | CT_START | CT_RESTART));
    Ok(())
}

/// The demod front end driven over the host face of the I2C, so [`dt295_core::fe1`] can
/// run its captured tune sequences blob-free. fe1 addresses the demods by their 8-bit
/// address (ISDB-T 0x20, ISDB-S 0x22); the host face wants the 7-bit form, so we shift.
#[allow(missing_debug_implementations)]
pub struct FjBus<'a, 'b> {
    io: &'b Io<'a>,
    /// Set once a transfer failed: a dead host bus, so [`fe1`]'s per-step `not_wedged`
    /// checks bail out instead of spinning every remaining op to its USB timeout.
    wedged: bool,
}

impl<'a, 'b> FjBus<'a, 'b> {
    pub fn new(io: &'b Io<'a>) -> Self {
        Self { io, wedged: false }
    }
}

impl fe1::Bus for FjBus<'_, '_> {
    fn i2c_write(&mut self, slave: u8, data: &[u8]) -> Result<()> {
        let mut rb: [u8; 0] = [];
        let r = xfer2(self.io, slave >> 1, data, &mut rb);
        if r.is_err() {
            self.wedged = true;
        }
        r
    }
    fn i2c_write_read(&mut self, slave: u8, wdata: &[u8], rbuf: &mut [u8]) -> Result<()> {
        let r = xfer2(self.io, slave >> 1, wdata, rbuf);
        if r.is_err() {
            self.wedged = true;
        }
        r
    }
    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    fn wedged(&self) -> bool {
        self.wedged
    }
}

/// Experimental blob-free COLD tune: host board bring-up, host-face I2C set-up, then run
/// fe1's captured front-end init + tune over the host face and read the lock / C/N. No
/// resident, no aux core, no vendor firmware. Proves the demod not only powers but locks
/// a real channel from cold. `restore` puts the board registers back afterwards.
pub fn cold_tune(
    io: &Io,
    terrestrial: bool,
    freq_hz: u32,
    tsid: Option<u16>,
    restore: bool,
) -> (Result<bool>, Vec<String>) {
    let mut log = Vec::new();
    let mut saved: Saved = Vec::new();
    let result = (|| {
        cold_bringup_host(io, &mut saved, &mut log)?;
        setup(io)?;
        log.push(format!(
            "I2C setup: CONTROL=0x{:04x} DIVIDER=0x{:04x}",
            r(io, O_CONTROL)?,
            r(io, O_DIVIDER)?
        ));
        let mut bus = FjBus::new(io);
        fe1::power_on_init(&mut bus)?;
        log.push("front-end init done".into());
        let sys = if terrestrial {
            fe1::tune_t(&mut bus, freq_hz)?;
            System::Terrestrial
        } else {
            let got = fe1::tune_s(&mut bus, freq_hz, tsid.unwrap_or(dt295_core::chan::NO_TSID))?;
            log.push(format!("tune_s selected TSID {got:?}"));
            System::Satellite
        };
        let st: Status = fe1::read_status(&mut bus, sys)?;
        log.push(format!(
            "status: locked={} C/N={}.{:03} dB raw=0x{:06x}",
            st.locked,
            st.cn_mdb / 1000,
            (st.cn_mdb % 1000).abs(),
            st.raw
        ));
        Ok(st.locked)
    })();
    if restore {
        cold_restore(io, &saved);
        log.push(format!("restored {} board registers", saved.len()));
    }
    (result, log)
}
