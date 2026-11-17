//! I2C transport over the MB86M01 type-9 command.
//!
//! Reproduces cmd_i2c_Write / cmd_i2c_WriteRead -> cmd_I2C_WrRd -> cmd_WorkerRoutine /
//! cmd_SendEntriesWaitAck and cmd_MsgReadI2CResp of the vendor driver:
//! * The front-end I2C is issued by the tuner state machine, whose command object is
//!   LSI command channel 1: doorbell bRequest 0xB9, ack in 0x080016. Channel 0 rejects
//!   type 9 with ack result 0x80.
//! * Each entry: write the 64-byte parameter block to 0x0800C0 (only when the entry
//!   carries data), then ring {9, sub, slave, len} and wait for the ack. Write = one
//!   entry, sub 0. Write+read = entry sub 2 (write part) then entry sub 3 (read part).
//! * After the last entry the firmware posts an I2C message: 0x090074 bit 8. The
//!   66-byte response at 0x0823BE is byte0/byte1 = status (both 0 == OK) followed by
//!   the read data from byte 2. Bit 8 of 0x082002 flags a failed message.

use tracing::{trace, warn};

use crate::proto::{Io, REG_IRQ, bytes_to_words};
use crate::{Error, Result};

/// Tuner state-machine command channel (doorbell 0xB9).
const I2C_CH: u8 = 1;
/// Type-9 parameter window (cmd_WriteParamBlock: 524480).
const PARAM_ADDR: u32 = 0x0800C0;
const PARAM_LEN: usize = 64;
/// I2C message (66 bytes).
const RESP_ADDR: u32 = 0x0823BE;
const RESP_LEN: usize = 66;
const IRQ_BIT_I2C_MSG: u32 = 8; // 0x0100
/// Per-message error flags.
const MSG_ERR_REG: u32 = 0x082002;
const ACK_TIMEOUT_MS: u32 = 1000;
/// cmd_WaitI2CMsg uses 1000 ms.
const MSG_TIMEOUT_MS: u32 = 1000;
/// Most bytes one entry carries.
const MAX_LEN: usize = 60;

/// W1C with a plain 16-bit write of just this bit, as the driver does (sub_14001CE70 ->
/// dev_RegWrite16); a masked write could clear other pending bits.
fn irq_clear(io: &Io, bit: u32) {
    let _ = io.write16(REG_IRQ, 1 << bit);
}

/// Poll the interrupt status register for `bit`; draining EP 0x83 doubles as the wait.
fn wait_irq_bit(io: &Io, bit: u32, timeout_ms: u32) -> (Result<()>, u16) {
    let mut waited = 0u32;
    loop {
        let s = match io.read16(REG_IRQ) {
            Ok(s) => s,
            Err(e) => return (Err(e.into_usb()), 0),
        };
        if s & (1 << bit) != 0 {
            return (Ok(()), s);
        }
        if waited >= timeout_ms {
            return (Err(Error::Timeout("I2C message")), s);
        }
        waited += if io.drain_interrupt(5).is_ok() { 1 } else { 5 };
    }
}

/// One command entry on channel 1: optional parameter block, then doorbell + ack.
fn entry(io: &mut Io, sub: u8, slave: u8, len: u8, wdata: Option<&[u8]>) -> Result<()> {
    if let Some(w) = wdata {
        let mut words = [0u8; PARAM_LEN];
        bytes_to_words(&mut words, w);
        io.write_block(PARAM_ADDR, &words)
            .map_err(Error::into_usb)?;
    }
    let rc = io.cmd_ack(
        I2C_CH,
        9,
        sub,
        (u16::from(slave) << 8) | u16::from(len),
        ACK_TIMEOUT_MS,
    );
    trace!(
        "  i2c entry {{9,{sub},{slave:02x},{len}}}: rc={}",
        rc.as_ref().map_or_else(Error::code, |_| 0)
    );
    rc
}

/// Wait for the I2C message and fetch the 66-byte response.
fn collect(io: &Io, slave: u8) -> Result<[u8; RESP_LEN]> {
    let (rc, s) = wait_irq_bit(io, IRQ_BIT_I2C_MSG, MSG_TIMEOUT_MS);
    if let Err(e) = rc {
        trace!("  i2c msg timeout (status=0x{s:04x})");
        return Err(e);
    }
    let err = io.read16(MSG_ERR_REG).unwrap_or(0);
    let mut resp = [0u8; RESP_LEN];
    let rc = io.read_block(RESP_ADDR, &mut resp);
    irq_clear(io, IRQ_BIT_I2C_MSG);
    rc.map_err(Error::into_usb)?;
    trace!(
        "  i2c msg err=0x{err:04x} resp: {}",
        hex_spaced(&resp[..40])
    );
    if err & (1 << IRQ_BIT_I2C_MSG) != 0 {
        warn!("i2c slave 0x{slave:02x}: message error flag (0x082002=0x{err:04x})");
        return Err(Error::Protocol(format!(
            "i2c slave 0x{slave:02x}: message error"
        )));
    }
    Ok(resp)
}

fn hex_spaced(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Plain write: entry {9, 0, slave, len} with the payload in the parameter window.
pub(crate) fn write(io: &mut Io, slave: u8, data: &[u8]) -> Result<()> {
    if data.len() > MAX_LEN {
        return Err(Error::Param("i2c write longer than 60 bytes"));
    }
    entry(io, 0, slave, data.len() as u8, Some(data))?;
    let resp = collect(io, slave)?;
    // A non-zero status is a NAK or the like.
    (resp[0] == 0).ok_or_else(|| {
        Error::Protocol(format!(
            "i2c slave 0x{slave:02x}: write status {:02x}",
            resp[0]
        ))
    })
}

/// Write then read: entry sub 2 (write part) + entry sub 3 (read part).
pub(crate) fn write_read(io: &mut Io, slave: u8, wdata: &[u8], rbuf: &mut [u8]) -> Result<()> {
    if wdata.len() > MAX_LEN || rbuf.len() > MAX_LEN {
        return Err(Error::Param("i2c transfer longer than 60 bytes"));
    }
    entry(io, 2, slave, wdata.len() as u8, Some(wdata))?;
    entry(io, 3, slave, rbuf.len() as u8, None)?;
    let resp = collect(io, slave)?;
    if resp[0] != 0 || resp[1] != 0 {
        return Err(Error::Protocol(format!(
            "i2c slave 0x{slave:02x}: read status {:02x}{:02x}",
            resp[0], resp[1]
        )));
    }
    let n = rbuf.len();
    rbuf.copy_from_slice(&resp[2..2 + n]);
    Ok(())
}
