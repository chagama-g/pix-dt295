//! PSI sections reassembled from TS packets, per PID, and their descriptor loops.

use super::packet::{self, PACKET};
use super::pat;

/// Sections of one PID being reassembled from its packets.
#[derive(Debug, Default)]
pub struct SectionBuf {
    cc: Option<u8>,
    pub buf: Vec<u8>,
    /// `buf` starts at a section boundary.
    synced: bool,
}

impl SectionBuf {
    /// Add one packet's payload; returns the sections it completes.
    pub fn push(&mut self, p: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let Some(off) = packet::payload_offset(p) else {
            return out;
        };
        let cc = packet::cc(p);
        match self.cc {
            Some(prev) if prev == cc => return out, // a duplicate packet
            Some(prev) if (prev + 1) & 15 != cc => {
                self.buf.clear();
                self.synced = false;
            }
            _ => {}
        }
        self.cc = Some(cc);
        let payload = &p[off..PACKET.min(p.len())];
        if packet::pusi(p) {
            let Some((&ptr, rest)) = payload.split_first() else {
                return out;
            };
            let ptr = usize::from(ptr).min(rest.len());
            if self.synced {
                self.buf.extend_from_slice(&rest[..ptr]);
                self.drain(&mut out);
            }
            self.buf.clear();
            self.buf.extend_from_slice(&rest[ptr..]);
            self.synced = true;
        } else if self.synced {
            self.buf.extend_from_slice(payload);
        }
        self.drain(&mut out);
        out
    }

    fn drain(&mut self, out: &mut Vec<Vec<u8>>) {
        while self.synced && self.buf.len() >= 3 {
            if self.buf[0] == 0xFF {
                // stuffing: the rest of the packet holds no section
                self.buf.clear();
                self.synced = false;
                break;
            }
            let n = pat::section_total(&self.buf);
            if self.buf.len() < n {
                break;
            }
            out.push(self.buf.drain(..n).collect());
        }
    }
}

/// (tag, body) of each descriptor in a descriptor loop.
pub fn descriptors(mut d: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    std::iter::from_fn(move || {
        let (&tag, rest) = d.split_first()?;
        let (&len, rest) = rest.split_first()?;
        let body = rest.get(..usize::from(len))?;
        d = &rest[usize::from(len)..];
        Some((tag, body))
    })
}
