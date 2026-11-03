//! MB86M01 register + command protocol over USB vendor requests.
//!
//! Confirmed from analysis of pxmb86m0x_usb.sys (docs/device.md):
//! * Register read/write: bRequest 0xBC, wValue = (addr >> 16) << 8 (| 1 for masked),
//!   wIndex = addr & 0xFFFF, data = 16-bit big-endian words.
//! * Command doorbell: bRequest 0xB8 (ch0) / 0xB9 (ch1), 4 data bytes
//!   {cmd, sub, par >> 8, par}.
//! * Firmware / command notifications arrive on interrupt EP 0x83 (6 bytes).

use std::fmt;
use std::time::{Duration, Instant};

use dt295_usb::Setup;
use tracing::{error, warn};

use crate::{Error, Result};

/// Firmware state register.
pub(crate) const REG_FW_STATE: u32 = 0x82008;
/// Interrupt / event status register (write-1-to-clear).
pub(crate) const REG_IRQ: u32 = 0x90074;

const REQ_REG: u8 = 0xBC;
const RT_IN: u8 = 0xC0;
const RT_OUT: u8 = 0x40;
const EP_INT: u8 = 0x83;

/// Consecutive command-ack timeouts after which the firmware counts as wedged.
pub(crate) const FW_STALL_LIMIT: u32 = 4;

/// Protocol state of one device: what every register / command exchange may need.
#[derive(Clone)]
pub(crate) struct Proto {
    /// CMDTIMER (5000 ms).
    pub cmd_timeout_ms: u32,
    /// Consecutive cmd-ack timeouts; at [`FW_STALL_LIMIT`] the firmware is wedged
    /// (stopped acking) and commands fail fast instead of a full timeout each. Reset
    /// on any ack; cleared by a (re)start.
    pub fw_stall: u32,
    /// Boot-auth session key (the K_SESS output), the key of the secure type-8 channel.
    pub sesskey: Option<[u8; 16]>,
    /// SECURE_REALTIME keepalive counter.
    pub ka_seq: u32,
    /// Time of the last SECURE_REALTIME keepalive (per device).
    pub ka_last: Option<Instant>,
    /// Consecutive failed keepalives (only the first is retried early).
    pub ka_fails: u32,
}

impl Default for Proto {
    fn default() -> Self {
        Proto {
            cmd_timeout_ms: 5000,
            fw_stall: 0,
            sesskey: None,
            ka_seq: 0,
            ka_last: None,
            ka_fails: 0,
        }
    }
}

impl fmt::Debug for Proto {
    // The session key does not belong in a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Proto")
            .field("cmd_timeout_ms", &self.cmd_timeout_ms)
            .field("fw_stall", &self.fw_stall)
            .field("sesskey", &self.sesskey.map(|_| "<set>"))
            .field("ka_seq", &self.ka_seq)
            .field("ka_last", &self.ka_last)
            .field("ka_fails", &self.ka_fails)
            .finish()
    }
}

/// Sleep for `ms` milliseconds (`usbx_sleep_ms`).
pub(crate) fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn ms(ms: u32) -> Duration {
    Duration::from_millis(u64::from(ms))
}

/// wValue / wIndex of a register access.
fn reg_setup(request_type: u8, addr: u32, masked: bool) -> Setup {
    Setup {
        request_type,
        request: REQ_REG,
        value: (((addr >> 16) << 8) as u16) | u16::from(masked),
        index: (addr & 0xFFFF) as u16,
    }
}

/// The masked-write transfer of a register field: (wValue, wIndex, payload).
///
/// The payload is value32 (BE) followed by mask32 (BE). The driver 4-aligns the
/// address; a 2-byte-offset field goes into bytes 2..3 (value) and 6..7 (mask). Only
/// the low 16 bits of the shifted value / mask are sent. wValue bit 0 marks the
/// masked form.
pub(crate) fn field_transfer(addr: u32, pos: u32, width: u32, val: u32) -> (u16, u16, [u8; 8]) {
    let mask = if width >= 32 {
        u32::MAX
    } else {
        (1u32 << width) - 1
    };
    let v = (val & mask).wrapping_shl(pos);
    let m = mask.wrapping_shl(pos);
    let aa = addr & !3;
    let off = if addr & 2 != 0 { 2 } else { 0 };
    let mut b = [0u8; 8];
    b[off] = (v >> 8) as u8;
    b[off + 1] = v as u8;
    b[4 + off] = (m >> 8) as u8;
    b[5 + off] = m as u8;
    let s = reg_setup(RT_OUT, aa, true);
    (s.value, s.index, b)
}

/// Byte -> BE16 word expansion of a parameter block (swap16_copy / sub_14003554C in the
/// vendor driver): each source byte becomes the high then low half of successive BE
/// words, i.e. a straight copy whose odd tail gets a zero low byte.
pub(crate) fn bytes_to_words(dst: &mut [u8], src: &[u8]) {
    for (i, &b) in src.iter().enumerate() {
        if i & 1 != 0 {
            dst[2 * (i >> 1) + 1] = b;
        } else {
            dst[2 * (i >> 1)] = b;
            dst[2 * (i >> 1) + 1] = 0;
        }
    }
}

/// One device's register / command access: the USB handle plus its protocol state.
pub(crate) struct Io<'a> {
    pub usb: &'a dt295_usb::Device,
    pub p: &'a mut Proto,
}

impl<'a> Io<'a> {
    pub(crate) fn new(usb: &'a dt295_usb::Device, p: &'a mut Proto) -> Self {
        Io { usb, p }
    }

    fn timeout(&self) -> Duration {
        ms(self.p.cmd_timeout_ms)
    }

    /// A vendor control OUT with the command timeout; returns the bytes sent.
    pub(crate) fn control_out(
        &self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<usize> {
        let s = Setup {
            request_type: RT_OUT,
            request,
            value,
            index,
        };
        Ok(self.usb.control_out(s, data, self.timeout())?)
    }

    fn expect_out(&self, s: Setup, data: &[u8]) -> Result<()> {
        let n = self.usb.control_out(s, data, self.timeout())?;
        if n == data.len() {
            Ok(())
        } else {
            Err(Error::Transfer(format!(
                "short control OUT ({n} of {})",
                data.len()
            )))
        }
    }

    pub(crate) fn read16(&self, addr: u32) -> Result<u16> {
        let mut b = [0u8; 2];
        let n = self
            .usb
            .control_in(reg_setup(RT_IN, addr, false), &mut b, self.timeout())?;
        if n != 2 {
            return Err(Error::Protocol(format!(
                "register 0x{addr:05x}: short read ({n})"
            )));
        }
        Ok(u16::from_be_bytes(b))
    }

    /// A 16-bit register read into a variable that is read again and again: store the value on success, keep the old one otherwise.
    pub(crate) fn read16_into(&self, addr: u32, v: &mut u16) -> bool {
        match self.read16(addr) {
            Ok(x) => {
                *v = x;
                true
            }
            Err(_) => false,
        }
    }

    pub(crate) fn write16(&self, addr: u32, val: u16) -> Result<()> {
        self.expect_out(reg_setup(RT_OUT, addr, false), &val.to_be_bytes())
    }

    /// Masked write of `width` bits at bit `pos` ([`field_transfer`]).
    pub(crate) fn write_field(&self, addr: u32, pos: u32, width: u32, val: u32) -> Result<()> {
        let (value, index, b) = field_transfer(addr, pos, width, val);
        self.expect_out(
            Setup {
                request_type: RT_OUT,
                request: REQ_REG,
                value,
                index,
            },
            &b,
        )
    }

    /// Block read: the MB86M01 stores 16-bit words; the host transfers big-endian word
    /// data in 64-byte chunks with the address advancing per chunk.
    pub(crate) fn read_block(&self, addr: u32, buf: &mut [u8]) -> Result<()> {
        for (i, chunk) in buf.chunks_mut(64).enumerate() {
            let a = addr + (i * 64) as u32;
            let want = chunk.len();
            let n = self
                .usb
                .control_in(reg_setup(RT_IN, a, false), chunk, self.timeout())?;
            if n != want {
                return Err(Error::Transfer(format!(
                    "short block read at 0x{a:05x} ({n} of {want})"
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn write_block(&self, addr: u32, data: &[u8]) -> Result<()> {
        for (i, chunk) in data.chunks(64).enumerate() {
            self.expect_out(reg_setup(RT_OUT, addr + (i * 64) as u32, false), chunk)?;
        }
        Ok(())
    }

    /// Ring the command doorbell of channel `ch` (bRequest 0xB8 ch0 / 0xB9 ch1).
    pub(crate) fn doorbell(&self, ch: u8, cmd: u8, sub: u8, par: u16) -> Result<()> {
        let [ph, pl] = par.to_be_bytes();
        let req = if ch != 0 { 0xB9 } else { 0xB8 };
        self.expect_out(
            Setup {
                request_type: RT_OUT,
                request: req,
                value: 0,
                index: 0,
            },
            &[cmd, sub, ph, pl],
        )
    }

    /// Drain one 6-byte interrupt packet (best effort; `Err(Timeout)` when none came).
    /// The state register is polled instead of parsing the payload, which the vendor
    /// driver also leaves uninspected.
    pub(crate) fn drain_interrupt(&self, timeout_ms: u32) -> Result<()> {
        let mut b = [0u8; 6];
        match self.usb.read(EP_INT, &mut b, ms(timeout_ms)) {
            Ok(_) => Ok(()),
            Err(dt295_usb::Error::Timeout) => Err(Error::Timeout("interrupt EP 0x83")),
            Err(e) => Err(e.into()),
        }
    }

    /// Ring the doorbell and wait for the command ack, as cmd_SendEntriesWaitAck /
    /// cmd_AckHandler do: the ack is bit 3 (ch0) / bit 1 (ch1) of the interrupt status
    /// 0x090074, and the per-channel ack register (ch0 0x080014, ch1 0x080016) holds the
    /// type echo (hi) and result (lo). Result 0x80 = the firmware rejected the command.
    /// The status bit is cleared with a plain 16-bit write of just that bit.
    pub(crate) fn cmd_ack(
        &mut self,
        ch: u8,
        cmd: u8,
        sub: u8,
        par: u16,
        timeout_ms: u32,
    ) -> Result<()> {
        let ackreg = if ch != 0 { 0x080016 } else { 0x080014 };
        let ackbit: u16 = if ch != 0 { 0x0002 } else { 0x0008 };
        // Once the firmware has wedged (stopped acking -- 0x090074 stuck with a pending
        // event the host never drained), every cmd_ack would otherwise burn a full
        // timeout, turning e.g. a tune's ~46 I2C ops into a 75 s hang. After a few
        // consecutive no-acks, fail immediately; a boot / re-plug clears the flag.
        if self.p.fw_stall >= FW_STALL_LIMIT {
            return Err(Error::Timeout("firmware wedged (not acking commands)"));
        }
        let _ = self.write16(REG_IRQ, ackbit);
        self.doorbell(ch, cmd, sub, par)?;
        let mut waited = 0u32;
        loop {
            let s = self.read16(REG_IRQ).map_err(Error::into_usb)?;
            if s & ackbit != 0 {
                break;
            }
            // Acknowledge a pending state-change event (bit 0x0004) so it does not block
            // this command's ack: the firmware waits for the host to drain it, and
            // leaving it (unlike the stream path) wedged the whole device mid-tune.
            if s & 0x0004 != 0 {
                let _ = self.write16(REG_IRQ, 0x0004);
            }
            if waited >= timeout_ms {
                self.p.fw_stall += 1;
                // A faulted firmware (0xFFFF) acks nothing more: wedged at once, so the
                // host recovers ~4 s sooner (a recorder gives a tuner command ~16 s).
                let fs = self.read16(REG_FW_STATE).unwrap_or(0);
                if fs == 0xFFFF {
                    self.p.fw_stall = FW_STALL_LIMIT;
                }
                if self.p.fw_stall == FW_STALL_LIMIT {
                    error!(
                        "firmware stopped acking commands (0x090074=0x{s:04x}, fw \
                         state=0x{fs:04x}); it is wedged -- failing fast until the device is \
                         started again"
                    );
                } else {
                    warn!("cmd ch{ch} {{{cmd},{sub},{par:04x}}}: no ack (status 0x{s:04x})");
                }
                return Err(Error::Timeout("command ack"));
            }
            waited += if self.drain_interrupt(5).is_ok() {
                1
            } else {
                5
            };
        }
        let a = self.read16(ackreg).unwrap_or(0);
        let _ = self.write16(REG_IRQ, ackbit);
        self.p.fw_stall = 0; // got an ack -> not wedged
        if (a >> 8) != u16::from(cmd) || (a & 0xFF) == 0x80 {
            warn!("cmd ch{ch} {{{cmd},{sub},{par:04x}}}: rejected (ack 0x{a:04x})");
            return Err(Error::Protocol(format!(
                "command {cmd}/{sub} rejected (ack 0x{a:04x})"
            )));
        }
        Ok(())
    }

    /// cmd_send: ring the doorbell, then wait for the firmware state register to reach
    /// `expect_state` (`None` = don't wait), polling every few ms. Mirrors
    /// cmd_WaitStateMsg of the vendor driver, by register polling rather than the
    /// interrupt message queue.
    pub(crate) fn cmd(
        &self,
        ch: u8,
        cmd: u8,
        sub: u8,
        par: u16,
        expect_state: Option<u16>,
        timeout_ms: u32,
    ) -> Result<()> {
        self.doorbell(ch, cmd, sub, par)?;
        // let the firmware pick up the doorbell
        let _ = self.drain_interrupt(50);
        let Some(want) = expect_state else {
            return Ok(());
        };
        let mut waited = 0u32;
        loop {
            if matches!(self.read16(REG_FW_STATE), Ok(st) if st == want) {
                return Ok(());
            }
            if waited >= timeout_ms {
                return Err(Error::Timeout("firmware state"));
            }
            sleep_ms(5);
            waited += 5;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// The control transfers of a register field write: known answers.
    #[test]
    fn field_write_transfers() {
        let cases: [(u32, u32, u32, u32, &str); 14] = [
            (0x90074, 3, 1, 1, "0901:0074=0008000000080000"),
            (0x90074, 2, 1, 1, "0901:0074=0004000000040000"),
            (0x1B02, 11, 2, 0, "0001:1b00=0000000000001800"),
            (0x1B02, 11, 2, 7, "0001:1b00=0000180000001800"),
            (0x101A, 15, 1, 1, "0001:1018=0000800000008000"),
            (0x1018, 0, 16, 0x0400, "0001:1018=04000000ffff0000"),
            (0x1018, 0, 16, 0x12345, "0001:1018=23450000ffff0000"),
            (0x80076, 12, 4, 1, "0801:0074=000010000000f000"),
            (0x80078, 8, 8, 0x1FF, "0801:0078=ff000000ff000000"),
            (0x90194, 12, 2, 1, "0901:0194=1000000030000000"),
            (0x90078, 13, 1, 1, "0901:0078=2000000020000000"),
            (0x901B4, 0, 1, 1, "0901:01b4=0001000000010000"),
            (0x82002, 4, 32, 0xFFFF_FFFF, "0801:2000=0000fff00000fff0"),
            (0x1B0E, 14, 2, 2, "0001:1b0c=000080000000c000"),
        ];
        for (addr, pos, width, val, want) in cases {
            let (v, i, b) = field_transfer(addr, pos, width, val);
            assert_eq!(
                format!("{v:04x}:{i:04x}={}", hex(&b)),
                want,
                "{addr:x} {pos} {width} {val:x}"
            );
        }
    }

    #[test]
    fn register_setup() {
        // w16 0x082028 / 0x90074 -> wValue 0x0800 / 0x0900
        let s = reg_setup(RT_OUT, 0x082028, false);
        assert_eq!((s.value, s.index), (0x0800, 0x2028));
        let s = reg_setup(RT_IN, 0x90074, false);
        assert_eq!(
            (s.request_type, s.request, s.value, s.index),
            (0xC0, 0xBC, 0x0900, 0x0074)
        );
        let s = reg_setup(RT_IN, 0x10, false);
        assert_eq!((s.value, s.index), (0x0000, 0x0010));
    }

    #[test]
    fn bytes_to_words_layout() {
        let mut dst = [0xEEu8; 16];
        bytes_to_words(&mut dst, &[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(hex(&dst), "0102030405060700eeeeeeeeeeeeeeee");
    }
}
