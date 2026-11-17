//! Low-level MB86M01 firmware download helpers: the pieces of init_LsiBoot /
//! fw_ColdDownloadAndBoot used by the vendor firmware boot and the System1 trc hot load
//! (in the `dt295` crate). The image is sent verbatim on bulk OUT EP 0x02 in <= 64 KiB
//! pieces.

use std::time::Duration;

use crate::Error;
use crate::proto::Io;

const EP_FW: u8 = 0x02;

/// Send an image on EP 0x02 in <= 64 KiB pieces (the trc hot load uses this too).
/// `Err((offset, error))` with the offset of the failed piece; the pipe halt is cleared.
pub fn bulk_download(io: &Io, img: &[u8]) -> std::result::Result<(), (usize, Error)> {
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
pub fn hostif_setup(io: &Io) {
    let _ = io.write_field(0x90194, 12, 2, 1);
    let _ = io.write16(0x90198, 0x0040);
}

/// Clear all command / stream interrupt enables (one bit per transfer).
pub fn irq_enable_all_off(io: &Io) {
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

pub fn irq_enable_on(io: &Io) {
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
pub fn write_idle_params(io: &Io) {
    let _ = io.write_field(0x80074, 8, 2, 1); // BCAS_NUM = 1
    let _ = io.write_field(0x80074, 0, 7, 0); // STOUTCLK = 0
    let _ = io.write_field(0x80076, 12, 4, 1); // STOUTCLK_DIV = 1
    let _ = io.write_field(0x80076, 8, 4, 1); // STOUT2CLK_DIV = 1
    let _ = io.write_field(0x80076, 4, 4, 1); // STOUT3CLK_DIV = 1
    let _ = io.write_field(0x80078, 8, 8, 0); // USB_NULL_OUT = 0
    let _ = io.write_field(0x80078, 0, 8, 0); // USB_NULL_OUT2 = 0
}
