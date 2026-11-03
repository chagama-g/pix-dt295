//! MB86M01 firmware download and boot: init_LsiBoot /
//! fw_ColdDownloadAndBoot / lsi_IdleInit of the vendor driver (docs/device.md).
//! The firmware file is sent verbatim on bulk OUT EP 0x02 in <= 64 KiB pieces.

use std::path::Path;
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};

use crate::proto::{Io, REG_FW_STATE, REG_IRQ, sleep_ms};
use crate::{Error, Result, auth};

const EP_FW: u8 = 0x02;
/// Size of the image header + trailer around the payload of an MB8AC031 file.
const HEADER_LEN: usize = 0x220;

/// Sanity check of the 32-byte MB8AC031 header: magic, and the payload length at 0x18
/// (BE) equal to the file size minus 0x220.
fn check_header(img: &[u8]) -> bool {
    if img.len() <= HEADER_LEN || &img[..8] != b"MB8AC031" {
        return false;
    }
    let paylen = u32::from_be_bytes([img[0x18], img[0x19], img[0x1A], img[0x1B]]);
    paylen as usize == img.len() - HEADER_LEN
}

/// Send an image on EP 0x02 in <= 64 KiB pieces (the trc hot load uses this too).
/// `Err((offset, error))` with the offset of the failed piece; the pipe halt is cleared.
pub(crate) fn bulk_download(io: &Io, img: &[u8]) -> std::result::Result<(), (usize, Error)> {
    for (i, chunk) in img.chunks(0x10000).enumerate() {
        let off = i * 0x10000;
        let e = match io.usb.bulk_out(EP_FW, chunk, Duration::from_millis(300)) {
            Ok(n) if n == chunk.len() => continue,
            Ok(n) => Error::Transfer(format!("short bulk OUT at {off} ({n} of {})", chunk.len())),
            Err(e) => e.into(),
        };
        let _ = io.usb.clear_halt(EP_FW);
        return Err((off, e));
    }
    Ok(())
}

/// Host-interface window setup, repeated before the download.
fn hostif_setup(io: &Io) {
    let _ = io.write_field(0x90194, 12, 2, 1);
    let _ = io.write16(0x90198, 0x0040);
}

/// Clear all command / stream interrupt enables (one bit per transfer).
fn irq_enable_all_off(io: &Io) {
    for p in (0..=13).rev() {
        let _ = io.write_field(0x90078, p, 1, 0);
        if p == 1 {
            let _ = io.write_field(0x90078, p, 1, 0); // written twice
        }
    }
    for p in (0..=11).rev() {
        let _ = io.write_field(0x901B4, p, 1, 0);
    }
}

fn irq_enable_on(io: &Io) {
    for p in (5..=13).rev() {
        let _ = io.write_field(0x90078, p, 1, 1);
    }
    let _ = io.write_field(0x90078, 4, 1, 0);
    for p in (0..=3).rev() {
        let _ = io.write_field(0x90078, p, 1, 1);
    }
    for p in (0..=11).rev() {
        let _ = io.write_field(0x901B4, p, 1, 1);
    }
}

/// IDLE_* parameters for the DT295 (single LSI). The stream stop writes them too.
pub(crate) fn write_idle_params(io: &Io) {
    let _ = io.write_field(0x80074, 8, 2, 1); // BCAS_NUM = 1
    let _ = io.write_field(0x80074, 0, 7, 0); // STOUTCLK = 0
    let _ = io.write_field(0x80076, 12, 4, 1); // STOUTCLK_DIV = 1
    let _ = io.write_field(0x80076, 8, 4, 1); // STOUT2CLK_DIV = 1
    let _ = io.write_field(0x80076, 4, 4, 1); // STOUT3CLK_DIV = 1
    let _ = io.write_field(0x80078, 8, 8, 0); // USB_NULL_OUT = 0
    let _ = io.write_field(0x80078, 0, 8, 0); // USB_NULL_OUT2 = 0
}

/// GPIO1 = tuner reset (active low): output enable, low 20 ms, high, 5 ms.
fn tuner_reset_pulse(io: &Io) {
    if let Ok(v) = io.read16(0x90014) {
        let _ = io.write16(0x90014, v | 0x0002);
    }
    if let Ok(v) = io.read16(0x90018) {
        let _ = io.write16(0x90018, v & !0x0002);
    }
    sleep_ms(20);
    if let Ok(v) = io.read16(0x90018) {
        let _ = io.write16(0x90018, v | 0x0002);
    }
    sleep_ms(5);
}

/// Write-1-to-clear the boot status bit (0x90074 bit 2) if it is set.
fn ack_boot_bit(io: &Io) {
    if matches!(io.read16(REG_IRQ), Ok(s) if s & 4 != 0) {
        let _ = io.write_field(REG_IRQ, 2, 1, 1);
    }
}

/// Idle init: authentication, IDLE params, command 1 -> state 0x11.
fn idle_init(io: &mut Io) -> Result<()> {
    let mut st = 0u16;
    if !io.read16_into(REG_FW_STATE, &mut st) || st != 0x10 {
        error!("idle_init: unexpected fw state 0x{st:04x}");
        return Err(Error::State(format!(
            "idle_init: firmware state 0x{st:04x}"
        )));
    }
    // make sure the command / response interrupt-status bits are enabled (the warm-start
    // path skips the post-download enable)
    irq_enable_on(io);

    // the firmware requires the AES-CMAC authentication handshake before it will leave
    // state 0x10 (verified on hardware)
    if let Err(e) = auth::perform(io) {
        warn!(
            "authentication handshake failed ({}); trying cmd1 anyway",
            e.code()
        );
        debug!("authentication handshake: {e}");
    }

    write_idle_params(io);
    io.read16_into(REG_FW_STATE, &mut st);
    info!("idle_init: state after IDLE params = 0x{st:04x}");

    // clear any stale STATE / ACK notifications on ch0
    let _ = io.write_field(REG_IRQ, 3, 1, 1);
    let _ = io.write_field(REG_IRQ, 1, 1, 1);

    // send command 1 (go-idle) and watch the state / status registers
    let _ = io.cmd(0, 1, 0, 0, None, io.p.cmd_timeout_ms);
    let (mut prev, mut s74) = (0xFFFFu16, 0u16);
    for i in 0..60 {
        let _ = io.drain_interrupt(50);
        io.read16_into(REG_FW_STATE, &mut st);
        io.read16_into(REG_IRQ, &mut s74);
        if st != prev || s74 & 0x0E != 0 {
            info!("  [{i:2}] state=0x{st:04x} status=0x{s74:04x}");
            prev = st;
        }
        if st == 0x11 {
            info!("idle_init: reached idle/ready (0x11)");
            return Ok(());
        }
        if s74 & 0x08 != 0 {
            let _ = io.write_field(REG_IRQ, 3, 1, 1); // ack STATE
        }
        if s74 & 0x02 != 0 {
            let _ = io.write_field(REG_IRQ, 1, 1, 1); // ack generic
        }
        sleep_ms(50);
    }
    error!(
        "idle_init: command 1 did not reach state 0x11 (state stays 0x{st:04x}) - firmware likely \
         requires the authentication handshake"
    );
    Err(Error::State(format!("idle_init: state stays 0x{st:04x}")))
}

/// Most bytes [`drain_ts`] reads (~1 s at the device's burst rate).
const DRAIN_MAX: usize = 32 << 20;

/// Read and discard what the TS endpoint still holds (at most `max` bytes).
fn drain_ts(io: &Io, max: usize) -> usize {
    let mut buf = vec![0u8; 0x10000];
    let mut total = 0;
    while total < max {
        match io
            .usb
            .read(crate::s1::EP_TS, &mut buf, Duration::from_millis(100))
        {
            Ok(n) if n > 0 => total += n,
            _ => break,
        }
    }
    total
}

/// Reset an already-running firmware back to the booted state 0x10 and re-run idle
/// init. Used when a previous run left the device in a stream state (0x01/0x02/0x04),
/// suspended (0x12), ready (0x11) or error (0xFFFF). Command 3 maps to the expected
/// state 0x10 from ANY state (cmd_ExpectedState), so it is a universal soft reset. Idle
/// init always runs afterwards so THIS process ends up holding a valid session key; a
/// re-download only works from a cold state 0 (there is no pre-download interrupt once
/// the firmware runs).
fn recover_to_idle(io: &mut Io, mut st: u16, fw_path: &Path) -> Result<()> {
    let tmo = io.p.cmd_timeout_ms;
    warn!("firmware already booted (state 0x{st:04x}); soft-resetting to idle");
    let _ = io.drain_interrupt(500);
    ack_boot_bit(io);
    // The TS of a stream left running when the secure session lapsed is still queued
    // (hundreds of KiB to MiB, encrypted with the old session's key); the next stream
    // would put it in front of its own TS. A stream stopped normally (below) is flushed
    // by its stop sequence.
    if st == 0xFFFF {
        let t0 = Instant::now();
        let n = drain_ts(io, DRAIN_MAX);
        if n > 0 {
            info!(
                "discarded {n} byte(s) of TS left from an earlier stream ({} ms)",
                t0.elapsed().as_millis()
            );
        }
    }
    // A stream an earlier process left running: it ended without stopping the stream
    // (SIGKILL; a recorder such as mirakc kills its tuner command that way). cmd3 does
    // not bring the firmware back from there; the stop sequence of a normal stream does.
    if matches!(st, 0x0001 | 0x0002 | 0x0004) {
        warn!("a stream was left running (state 0x{st:04x}); stopping it");
        crate::s1::to_idle(io);
        if let Ok(now) = io.read16(REG_FW_STATE) {
            st = now;
        }
    }
    // From idle (0x11), as the vendor driver starts on a running firmware: reload the
    // idle image in place and authenticate anew. Without the reload the firmware keeps
    // the previous process's secure session, and its timeout faults the new one when it
    // falls inside it (an open about a minute after the previous close: commands no
    // longer acked, state 0xFFFF).
    if st == 0x11 {
        match reload_idle(io, fw_path) {
            Ok(()) => return idle_init(io),
            Err(e) => {
                warn!("idle firmware reload failed ({e}); suspending and resuming instead");
                if let Ok(now) = io.read16(REG_FW_STATE) {
                    st = now;
                }
                if st == 0x10 {
                    return idle_init(io);
                }
            }
        }
    }
    // From idle (0x11) the firmware rejects both cmd3 and cmd(2,0,0) (ack result 0x80,
    // verified on hardware). The legal path is idle -> suspend with cmd(2,1,0) (-> 0x12),
    // then cmd(2,0,0) (-> 0x10) (cmd_ExpectedState: cmd2 sub1 -> 0x12, sub0 -> 0x10).
    // cmd3 stays the recovery for the error / stream states.
    if st == 0x11 || st == 0x12 {
        let _ = io.write16(REG_IRQ, 0x0004);
        if st == 0x11 && io.cmd_ack(0, 2, 1, 0, tmo).is_ok() {
            sleep_ms(100); // -> 0x12
        }
        let _ = io.write16(REG_IRQ, 0x0004);
        if io.cmd_ack(0, 2, 0, 0, tmo).is_ok() {
            for _ in (0..1000).step_by(20) {
                if io.read16_into(REG_FW_STATE, &mut st) && st == 0x10 {
                    break;
                }
                sleep_ms(20);
            }
        }
        let _ = io.write16(REG_IRQ, 0x0004);
        if io.read16_into(REG_FW_STATE, &mut st) && st == 0x10 {
            return idle_init(io);
        }
        warn!("suspend/resume left state 0x{st:04x}; trying cmd3");
    }
    let _ = io.cmd(0, 3, 0, 0, Some(0x10), tmo);
    if !(io.read16_into(REG_FW_STATE, &mut st) && st == 0x10) {
        // one more try after draining any late notifications
        let _ = io.drain_interrupt(300);
        ack_boot_bit(io);
        let _ = io.cmd(0, 3, 0, 0, Some(0x10), tmo);
        if !(io.read16_into(REG_FW_STATE, &mut st) && st == 0x10) {
            error!("soft reset failed (state 0x{st:04x}); please re-plug the device");
            return Err(Error::Firmware(format!(
                "soft reset failed (state 0x{st:04x})"
            )));
        }
    }
    idle_init(io)
}

/// The idle firmware image, its header checked.
fn read_image(fw_path: &Path) -> Result<Vec<u8>> {
    let img = match std::fs::read(fw_path) {
        Ok(img) if img.len() > HEADER_LEN => img,
        other => {
            error!("cannot read firmware '{}'", fw_path.display());
            return Err(match other {
                Err(e) => e.into(),
                Ok(_) => Error::Firmware(format!("'{}' is too small", fw_path.display())),
            });
        }
    };
    if !check_header(&img) {
        error!("firmware header invalid (need mb86m01_assp_osec_idle.bin)");
        return Err(Error::Firmware("firmware header invalid".into()));
    }
    Ok(img)
}

/// Reload the idle firmware of a running device in place (the vendor driver's step
/// when it starts and finds the firmware at 0x11): cmd(4,0,1), the image on EP 0x02
/// as the trc hot load sends its own, cmd(4,1,0) -> 0x10. Authentication follows.
fn reload_idle(io: &mut Io, fw_path: &Path) -> Result<()> {
    let img = read_image(fw_path)?;
    let tmo = io.p.cmd_timeout_ms;
    let t0 = Instant::now();
    let _ = io.write16(REG_IRQ, 0x0004);
    io.cmd_ack(0, 4, 0, 1, tmo)?;
    bulk_download(io, &img).map_err(|(_, e)| e)?;
    let _ = io.write16(REG_IRQ, 0x0004);
    io.cmd(0, 4, 1, 0, Some(0x10), tmo)?;
    info!(
        "idle firmware reloaded ({} B, {} ms)",
        img.len(),
        t0.elapsed().as_millis()
    );
    Ok(())
}

/// The cold-plug path: download `fw_path` and boot it (then idle init if needed).
fn cold_boot(io: &mut Io, fw_path: &Path) -> Result<()> {
    let img = read_image(fw_path)?;

    hostif_setup(io);
    irq_enable_all_off(io);

    let mut st = 0u16;
    // The interrupt only announces the status bit, and it comes once: an earlier read of
    // the endpoint (another program, a test) may have taken it. The bit decides.
    let notified = io.drain_interrupt(500).is_ok();
    if !io.read16_into(REG_IRQ, &mut st) || st & 4 == 0 {
        if notified {
            error!("pre-download status bit not set (0x90074=0x{st:04x})");
            return Err(Error::Protocol("pre-download status bit not set".into()));
        }
        error!("no pre-download interrupt from device (0x90074=0x{st:04x})");
        return Err(Error::Timeout("pre-download interrupt"));
    }
    if !notified {
        warn!("no pre-download interrupt, but its status bit is set (0x90074=0x{st:04x})");
    }
    let _ = io.write_field(REG_IRQ, 2, 1, 1); // W1C

    info!("downloading firmware ({} bytes)...", img.len());
    if let Err((_, e)) = bulk_download(io, &img) {
        error!("firmware bulk download failed");
        return Err(e);
    }

    irq_enable_on(io);
    let _ = io.write_field(0x90070, 2, 1, 1); // start doorbell

    if io.drain_interrupt(2000).is_err() {
        error!("no post-download boot interrupt");
        return Err(Error::Timeout("post-download boot interrupt"));
    }
    if !io.read16_into(REG_IRQ, &mut st) || st & 4 == 0 {
        error!("post-download status bit not set");
        return Err(Error::Protocol("post-download status bit not set".into()));
    }
    if !io.read16_into(REG_FW_STATE, &mut st) || (st != 0x10 && st != 0x11) {
        error!("firmware did not boot (state 0x{st:04x})");
        return Err(Error::Firmware(format!(
            "firmware did not boot (state 0x{st:04x})"
        )));
    }
    let _ = io.write_field(REG_IRQ, 2, 1, 1); // W1C
    info!("firmware booted, state = 0x{st:04x}");

    // version / sanity block at register 0x10; word 3's top nibble must be 0
    let mut ver = [0u8; 8];
    if io.read_block(0x10, &mut ver).is_ok() && (ver[6] >> 4) != 0 {
        error!("firmware version nibble check failed");
        return Err(Error::Firmware(
            "firmware version nibble check failed".into(),
        ));
    }
    if st == 0x10 {
        idle_init(io)?;
    }
    Ok(())
}

/// Boot the firmware: download it on a cold device (state 0), otherwise soft-reset the
/// running firmware to idle; then reset the tuner and start the B-CAS init. A cold boot
/// (fresh plug) drops `prior_link_keys`: a key from before the power cycle no longer
/// applies.
pub(crate) fn boot(io: &mut Io, fw_path: &Path, prior_link_keys: &mut Vec<[u8; 16]>) -> Result<()> {
    hostif_setup(io);
    let st = io.read16(REG_FW_STATE).map_err(Error::into_usb)?;
    info!("firmware state before boot = 0x{st:04x}");
    match st {
        // downloaded but not yet initialised (e.g. a previous run that stopped right
        // after boot): just init it
        0x10 => idle_init(io)?,
        // cold plug: the full firmware download
        0 => {
            prior_link_keys.clear();
            cold_boot(io, fw_path)?;
        }
        // Already running, left in a stream / ready / suspend / error state by a
        // previous run: soft-reset to idle (a download only works from state 0 and
        // otherwise fails at the "no pre-download interrupt" check).
        _ => recover_to_idle(io, st, fw_path)?,
    }
    tuner_reset_pulse(io);
    // B-CAS init command on channel 1 (cmd 0x0A); failure is ignored (TS-only use)
    let _ = io.cmd(1, 0x0A, 0, 1, None, io.p.cmd_timeout_ms);
    info!("firmware ready");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(paylen: u32, total: usize) -> Vec<u8> {
        let mut v = vec![0u8; total];
        v[..8].copy_from_slice(b"MB8AC031");
        v[0x18..0x1C].copy_from_slice(&paylen.to_be_bytes());
        v
    }

    #[test]
    fn header_check() {
        assert!(check_header(&image(0x100, 0x220 + 0x100)));
        assert!(!check_header(&image(0x101, 0x220 + 0x100)));
        assert!(!check_header(&image(0, 0x220)));
        let mut bad = image(0x100, 0x320);
        bad[0] = b'X';
        assert!(!check_header(&bad));
        assert!(!check_header(b"MB8AC031"));
    }
}
