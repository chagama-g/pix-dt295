//! Parameter checks shared by both backends.

use crate::{Error, Result};

/// Bit 7 of an endpoint address / `bmRequestType`: device-to-host.
pub(crate) const fn is_in(addr: u8) -> bool {
    addr & 0x80 != 0
}

/// `read_queue` parameters, checked as `usbx_rq_open` does: a bulk IN endpoint, at
/// least one request, and a size that is a non-zero multiple of 512 (the high-speed
/// bulk packet size -- RAW_IO and usbfs both want whole packets).
pub(crate) fn check_read_queue(ep: u8, n: usize, size: usize) -> Result<()> {
    if !is_in(ep) || n == 0 || size == 0 || !size.is_multiple_of(512) {
        return Err(Error::Param);
    }
    Ok(())
}

/// A control transfer's direction (bit 7 of `bmRequestType`) must match the call:
/// an IN request on `control_out` would let the device write into a shared slice.
pub(crate) fn check_control_dir(request_type: u8, want_in: bool) -> Result<()> {
    (is_in(request_type) == want_in).ok_or(Error::Param)
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    #[test]
    fn read_queue_params() {
        assert!(check_read_queue(0x81, 8, 0x10000).is_ok());
        assert!(check_read_queue(0x81, 1, 512).is_ok());
        assert_matches!(
            check_read_queue(0x02, 8, 0x10000),
            Err(Error::Param),
            "OUT endpoint"
        );
        assert_matches!(
            check_read_queue(0x81, 0, 0x10000),
            Err(Error::Param),
            "n == 0"
        );
        assert_matches!(check_read_queue(0x81, 8, 0), Err(Error::Param), "size == 0");
        assert_matches!(
            check_read_queue(0x81, 8, 1000),
            Err(Error::Param),
            "size % 512"
        );
        assert_matches!(check_read_queue(0x81, 8, 0x10000 + 188), Err(Error::Param));
    }

    #[test]
    fn control_direction() {
        assert!(check_control_dir(0xC0, true).is_ok());
        assert!(check_control_dir(0x40, false).is_ok());
        assert_matches!(check_control_dir(0x40, true), Err(Error::Param));
        assert_matches!(check_control_dir(0xC0, false), Err(Error::Param));
    }

    #[test]
    fn device_is_send_sync_and_queue_is_send() {
        fn send_sync<T: Send + Sync>() {}
        fn send<T: Send>() {}
        send_sync::<crate::Device>();
        send::<crate::ReadQueue<'static>>();
    }

    #[test]
    fn access_error_carries_the_winusb_hint() {
        assert_eq!(
            Error::Access.to_string(),
            "access denied (is the device bound to WinUSB?)"
        );
    }
}
