//! The PMT: what a program carries, and the B-CAS ECMs of its scrambled PIDs.

use super::section::descriptors;
use super::{crc, packet};

/// table_id of the PMT.
pub(crate) const TID_PMT: u8 = 0x02;
/// The CA_descriptor, and the B-CAS CA_system_id.
pub(crate) const TAG_CA: u8 = 0x09;
const CA_BCAS: u16 = 0x0005;

/// One program's PMT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pmt {
    pub program: u16,
    pub pcr: u16,
    /// The program-level ECM.
    pub ecm: Option<u16>,
    /// (PID, stream_type, ECM)
    pub es: Vec<(u16, u8, Option<u16>)>,
}

/// MPEG-1/2 video, H.264, H.265; MPEG-1/2 audio, AAC (ADTS, LATM).
pub(crate) fn is_av(stream_type: u8) -> bool {
    matches!(
        stream_type,
        0x01 | 0x02 | 0x1B | 0x24 | 0x03 | 0x04 | 0x0F | 0x11
    )
}

/// The ECM PID of the B-CAS CA_descriptor in a descriptor loop (0x1FFF: explicitly not
/// scrambled).
fn ca_in(d: &[u8]) -> Option<u16> {
    descriptors(d).find_map(|(tag, b)| {
        (tag == TAG_CA && b.len() >= 4 && u16::from_be_bytes([b[0], b[1]]) == CA_BCAS)
            .then(|| u16::from_be_bytes([b[2], b[3]]) & 0x1FFF)
    })
}

/// A CRC-valid, current PMT section.
pub(crate) fn parse_pmt(sec: &[u8]) -> Option<Pmt> {
    if sec.len() < 16 || sec[0] != TID_PMT || sec[5] & 1 == 0 || !crc::trailing_crc_ok(sec) {
        return None;
    }
    let program = u16::from_be_bytes([sec[3], sec[4]]);
    let pcr = u16::from_be_bytes([sec[8], sec[9]]) & 0x1FFF;
    let info_len = (usize::from(sec[10] & 0x0F) << 8) | usize::from(sec[11]);
    let end = sec.len() - 4;
    let ecm = ca_in(sec.get(12..12 + info_len)?).filter(|&p| p != packet::NULL_PID);
    let mut es = Vec::new();
    let mut i = 12 + info_len;
    while i + 5 <= end {
        let st = sec[i];
        let pid = u16::from_be_bytes([sec[i + 1], sec[i + 2]]) & 0x1FFF;
        let len = (usize::from(sec[i + 3] & 0x0F) << 8) | usize::from(sec[i + 4]);
        let ca = ca_in(sec.get(i + 5..(i + 5 + len).min(end))?);
        es.push((
            pid,
            st,
            ca.map_or(ecm, |p| (p != packet::NULL_PID).then_some(p)),
        ));
        i += 5 + len;
    }
    Some(Pmt {
        program,
        pcr,
        ecm,
        es,
    })
}
