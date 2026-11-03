//! PAT checks (ports of `s1_pat_section`, `s1_check_pat`, `find_section`,
//! and the PAT parsing in `s1_read_pat`).

use super::crc;
use super::packet::{self, PACKET, SYNC};

/// The section reassembly buffer of `find_section` (larger than any section).
const FIND_ACC_MAX: usize = 4300;

/// table_id of the PAT.
const TID_PAT: u8 = 0x00;

/// 3 + section_length: the whole length a section header announces.
pub(crate) fn section_total(hdr: &[u8]) -> usize {
    3 + ((usize::from(hdr[1] & 0x0F) << 8) | usize::from(hdr[2]))
}

/// Every (program_number, PID) entry of a PAT section's loop, program 0 (the NIT
/// entry) included; the loop runs from offset 8 to the CRC.
pub(crate) fn pat_entries(sec: &[u8]) -> impl Iterator<Item = (u16, u16)> + '_ {
    let end = sec.len().saturating_sub(4);
    sec.get(8..end)
        .unwrap_or_default()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|e| {
            (
                u16::from_be_bytes([e[0], e[1]]),
                u16::from_be_bytes([e[2], e[3]]) & 0x1FFF,
            )
        })
}

/// The PAT section of one PID-0 packet if it starts there (PUSI), fits the packet
/// and is CRC-valid.
pub fn pat_section(p: &[u8]) -> Option<&[u8]> {
    let q = p.get(..PACKET)?;
    if !packet::pusi(q) {
        return None;
    }
    let s = packet::payload_offset(q)?;
    let o = s + 1 + usize::from(q[s]); // skip the pointer_field
    let hdr = q.get(o..o + 3)?;
    if hdr[0] != TID_PAT {
        return None;
    }
    let total = section_total(hdr);
    // section_length >= 9: tsid .. last_section_number (5) + CRC (4); and it must fit
    if total < 12 {
        return None;
    }
    let sec = q.get(o..o + total)?;
    crc::trailing_crc_ok(sec).then_some(sec)
}

/// For a CRC-valid single-packet PAT: whether it lists `program`. `None` if the
/// packet holds no such PAT.
pub fn pat_lists(p: &[u8], program: u16) -> Option<bool> {
    let sec = pat_section(p)?;
    Some(pat_entries(sec).any(|(prog, _)| prog == program))
}

/// The first CRC-valid section on `pid` reassembled from a decrypted, 188-aligned
/// buffer.
///
/// Only clear (TSC 00) packets count. A section start drops whatever was being
/// assembled, and a complete section with a bad CRC is discarded until the next
/// section start -- enough to pick a PAT/PMT out of a capture of many repeats.
pub fn find_section(buf: &[u8], pid: u16) -> Option<Vec<u8>> {
    let mut acc: Vec<u8> = Vec::new();
    let mut have = false;
    for q in buf.as_chunks::<PACKET>().0 {
        if q[0] != SYNC || packet::pid(q) != pid || packet::tsc(q) != 0 {
            continue;
        }
        let Some(s) = packet::payload_offset(q) else {
            continue;
        };
        let mut body = &q[s..];
        if packet::pusi(q) {
            let ptr = usize::from(body[0]);
            let Some(rest) = body.get(1 + ptr..) else {
                continue;
            };
            acc.clear();
            have = true;
            body = rest;
        } else if !have {
            continue;
        }
        let room = FIND_ACC_MAX - acc.len();
        acc.extend_from_slice(&body[..body.len().min(room)]);
        if acc.len() >= 3 {
            let total = section_total(&acc);
            if acc.len() >= total {
                if crc::trailing_crc_ok(&acc[..total]) {
                    acc.truncate(total);
                    return Some(acc);
                }
                have = false;
                acc.clear();
            }
        }
    }
    None
}

/// (program_number, PMT PID) of a PAT section, program 0 (NIT) excluded.
///
/// Anything but a PAT (table_id 0) of at least the minimal 12 bytes gives nothing.
pub fn parse_pat(sec: &[u8]) -> Vec<(u16, u16)> {
    if sec.len() < 12 || sec[0] != TID_PAT {
        return Vec::new();
    }
    pat_entries(sec).filter(|&(prog, _)| prog != 0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::testutil::{make_pat_packet, packetize};

    /// NHK-like terrestrial PAT: NIT, two TV services and the one-seg service.
    const NHK: [(u16, u16); 4] = [(0, 0x0010), (1024, 0x01F0), (1025, 0x03F0), (1408, 0x1FC8)];

    // ---- PAT checks ----

    #[test]
    fn tuned_sid_listed() {
        let q = make_pat_packet(&NHK, 0, false);
        assert_eq!(pat_lists(&q, 1024), Some(true));
    }

    #[test]
    fn tuned_sid_absent() {
        let q = make_pat_packet(&NHK, 0, false);
        assert_eq!(pat_lists(&q, 1040), Some(false));
    }

    #[test]
    fn program_0_matches_only_if_asked() {
        let q = make_pat_packet(&NHK, 0, false);
        assert_eq!(pat_lists(&q, 0), Some(true));
    }

    #[test]
    fn bad_crc_not_counted() {
        let q = make_pat_packet(&NHK, 0, true);
        assert_eq!(pat_lists(&q, 1040), None);
        assert_eq!(pat_lists(&q, 1024), None);
    }

    #[test]
    fn pointer_field_skipped() {
        let q = make_pat_packet(&NHK, 7, false);
        assert_eq!(pat_lists(&q, 1025), Some(true));
    }

    #[test]
    fn no_pusi_ignored() {
        let mut q = make_pat_packet(&NHK, 0, false);
        q[1] = 0x00;
        assert_eq!(pat_lists(&q, 1024), None);
    }

    #[test]
    fn pointer_past_packet_ignored_safely() {
        let mut q = make_pat_packet(&NHK, 0, false);
        q[4] = 250;
        assert_eq!(pat_lists(&q, 1024), None);
        q[4] = 182;
        assert_eq!(pat_lists(&q, 1024), None);
    }

    // ---- more cases ----

    #[test]
    fn pat_section_bounds() {
        let q = make_pat_packet(&NHK, 0, false);
        let sec = pat_section(&q).expect("valid PAT");
        assert_eq!(sec.len(), 8 + 4 * NHK.len() + 4);
        assert_eq!(sec[0], 0x00);
        assert!(pat_section(&q[..187]).is_none(), "short packet");
        // a section longer than the packet
        let mut long = q;
        long[6] = 0xB0 | 0x0F;
        assert!(pat_section(&long).is_none());
        // section_length < 9
        let mut tiny = q;
        tiny[6] = 0xB0;
        tiny[7] = 8;
        assert!(pat_section(&tiny).is_none());
        // not a PAT
        let mut pmt = q;
        pmt[5] = 0x02;
        assert!(pat_section(&pmt).is_none());
        // adaptation field: the payload (pointer_field) follows it
        let mut af = [0xFFu8; PACKET];
        af[..4].copy_from_slice(&[SYNC, 0x40, 0x00, 0x30]);
        af[4] = 10;
        af[5] = 0x00;
        let sec_len = sec.len();
        af[15] = 0; // pointer_field
        af[16..16 + sec_len].copy_from_slice(sec);
        assert_eq!(pat_section(&af), Some(sec));
        af[3] = 0x20; // adaptation field only
        assert!(pat_section(&af).is_none());
    }

    #[test]
    fn parse_pat_skips_nit() {
        let q = make_pat_packet(&NHK, 3, false);
        let sec = pat_section(&q).unwrap();
        assert_eq!(
            parse_pat(sec),
            vec![(1024, 0x01F0), (1025, 0x03F0), (1408, 0x1FC8)]
        );
        let mut not_pat = sec.to_vec();
        not_pat[0] = 0x02;
        assert!(parse_pat(&not_pat).is_empty());
        assert!(parse_pat(&sec[..11]).is_empty());
    }

    #[test]
    fn find_section_reassembles_across_packets() {
        // a 400-byte section on PID 0x0101, split over three packets, after noise
        let mut sec = vec![0x02, 0xB0, 0x00, 0x00, 0x01, 0xC1, 0, 0];
        sec.extend((0..388).map(|i| i as u8));
        sec.extend([0; 4]);
        let sl = sec.len() - 3;
        sec[1] = 0xB0 | (sl >> 8) as u8;
        sec[2] = sl as u8;
        crc::set_section_crc(&mut sec);
        let mut cc = 0;
        let mut buf = packetize(0x0200, &sec, &mut cc, 0); // another PID
        let pkts = packetize(0x0101, &sec, &mut cc, 0);
        buf.extend_from_slice(&pkts[PACKET..2 * PACKET]); // a continuation without its start
        buf.extend(packetize(0x0101, &sec, &mut cc, 5));
        assert_eq!(find_section(&buf, 0x0101).as_deref(), Some(&sec[..]));
        assert_eq!(find_section(&buf, 0x0200).as_deref(), Some(&sec[..]));
        assert_eq!(find_section(&buf, 0x0300), None);
    }

    #[test]
    fn find_section_skips_bad_crc_and_scrambled() {
        let good = make_pat_packet(&NHK, 0, false);
        let bad = make_pat_packet(&NHK, 0, true);
        let mut enc = good;
        enc[3] |= 0x40; // TSC 01: still link-encrypted
        let mut buf = Vec::new();
        buf.extend_from_slice(&bad);
        buf.extend_from_slice(&enc);
        assert_eq!(find_section(&buf, 0), None);
        buf.extend_from_slice(&good);
        assert_eq!(find_section(&buf, 0).as_deref(), pat_section(&good));
    }

    #[test]
    fn find_section_ignores_trailing_partial_packet() {
        let good = make_pat_packet(&NHK, 0, false);
        assert!(find_section(&good[..187], 0).is_none());
        let mut lost = good;
        lost[0] = 0x00; // lost sync byte
        assert!(find_section(&lost, 0).is_none());
    }
}
