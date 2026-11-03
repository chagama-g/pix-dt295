//! TS packets of a byte stream, for the sections of [`SectionBuf`].

use dt295::ts::packet::{self, PACKET, SYNC};
pub(crate) use dt295::ts::section::SectionBuf;

/// Splits a byte stream into TS packets: finds the sync byte, and keeps a packet cut
/// at the end of one `feed` for the next.
#[derive(Debug, Default)]
pub(crate) struct Packets {
    carry: Vec<u8>,
}

impl Packets {
    /// Calls `f` with each whole packet of the bytes kept and `data`.
    pub(crate) fn feed(&mut self, data: &[u8], mut f: impl FnMut(&[u8])) {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(data);
        let mut pos = 0;
        while pos + PACKET <= buf.len() {
            if buf[pos] != SYNC {
                match packet::find_sync(&buf[pos..]) {
                    Some(s) => pos += s,
                    None => {
                        // keep a tail that may sync with the next call's data
                        pos = pos.max(buf.len().saturating_sub(4 * PACKET - 1));
                        break;
                    }
                }
            }
            f(&buf[pos..pos + PACKET]);
            pos += PACKET;
        }
        self.carry = buf.split_off(pos.min(buf.len()));
    }
}
