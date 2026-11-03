//! Per-stream statistics from packet headers, taken BEFORE link decryption (the
//! header is never encrypted): packets, link-encrypted packets, PID-0 / SDT packets,
//! distinct PIDs, continuity errors in the device's TS, TEI packets, and one raw
//! PID-0 section-start packet (for the "PSI present but undecryptable" diagnosis).

use std::fmt;

use super::packet::{self, NULL_PID, PACKET, SYNC};

/// PIDs are 13 bits.
const PIDS: usize = 8192;
/// "No continuity_counter seen yet" in [`RawStats`]'s per-PID table.
const CC_NONE: u8 = 0xFF;
/// The SDT / BAT PID.
const SDT_PID: u16 = 0x0011;

/// Header statistics of one stream.
#[derive(Clone)]
pub struct RawStats {
    pub packets: u64,
    pub link_encrypted: u64,
    pub pid0_packets: u64,
    pub sdt_packets: u64,
    pub distinct_pids: u32,
    pub cc_errors: u64,
    pub tei_packets: u64,
    /// The first PID-0 packet with PUSI, as received.
    pub raw_pat: Option<[u8; 188]>,
    /// Last continuity_counter per PID, [`CC_NONE`] before the first payload packet.
    cc_last: Box<[u8; PIDS]>,
    /// One bit per PID seen.
    pid_seen: Box<[u8; PIDS / 8]>,
}

impl RawStats {
    pub fn new() -> Self {
        Self {
            packets: 0,
            link_encrypted: 0,
            pid0_packets: 0,
            sdt_packets: 0,
            distinct_pids: 0,
            cc_errors: 0,
            tei_packets: 0,
            raw_pat: None,
            cc_last: Box::new([CC_NONE; PIDS]),
            pid_seen: Box::new([0; PIDS / 8]),
        }
    }

    /// Account a 188-aligned buffer as received from the device. `on_cc_error` is
    /// called with the PID of every continuity error (payload packets count up mod
    /// 16; a repeat is a legal duplicate; a discontinuity_indicator restarts the
    /// count; PID 0x1FFF is ignored). Returns the number of TEI packets in `buf`.
    ///
    /// Packets without a sync byte are skipped, as is a trailing partial packet.
    /// TEI marks what the demodulator could not correct; a packet the device drops
    /// instead shows up as a continuity error.
    pub fn observe(&mut self, buf: &[u8], mut on_cc_error: impl FnMut(u16)) -> u64 {
        let mut tei = 0;
        for q in buf.as_chunks::<PACKET>().0 {
            if q[0] != SYNC {
                continue;
            }
            let pid = packet::pid(q);
            let ix = usize::from(pid);
            self.packets += 1;
            if packet::tei(q) {
                tei += 1;
            }
            let afc = packet::afc(q);
            if afc & 1 != 0 && pid != NULL_PID {
                let cc = packet::cc(q);
                let last = self.cc_last[ix];
                let disc = afc == 3 && q[4] > 0 && q[5] & 0x80 != 0;
                if last != CC_NONE && !disc && cc != last && cc != (last + 1) & 0x0F {
                    self.cc_errors += 1;
                    on_cc_error(pid);
                }
                self.cc_last[ix] = cc;
            }
            if packet::tsc(q) == 0x01 {
                self.link_encrypted += 1;
            }
            let (byte, bit) = (ix >> 3, 1u8 << (ix & 7));
            if self.pid_seen[byte] & bit == 0 {
                self.pid_seen[byte] |= bit;
                self.distinct_pids += 1;
            }
            if pid == 0x0000 {
                self.pid0_packets += 1;
                if self.raw_pat.is_none() && packet::pusi(q) {
                    self.raw_pat = Some(*q);
                }
            } else if pid == SDT_PID {
                self.sdt_packets += 1;
            }
        }
        self.tei_packets += tei;
        tei
    }
}

impl Default for RawStats {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for RawStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawStats")
            .field("packets", &self.packets)
            .field("link_encrypted", &self.link_encrypted)
            .field("pid0_packets", &self.pid0_packets)
            .field("sdt_packets", &self.sdt_packets)
            .field("distinct_pids", &self.distinct_pids)
            .field("cc_errors", &self.cc_errors)
            .field("tei_packets", &self.tei_packets)
            .field("raw_pat", &self.raw_pat.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload-only packet on `pid` with continuity counter `cc`.
    fn pkt(pid: u16, cc: u8) -> [u8; PACKET] {
        let mut p = [0xFFu8; PACKET];
        p[..4].copy_from_slice(&[SYNC, (pid >> 8) as u8, pid as u8, 0x10 | (cc & 0x0F)]);
        p
    }

    fn run(stats: &mut RawStats, pkts: &[[u8; PACKET]]) -> Vec<u16> {
        let mut errs = Vec::new();
        stats.observe(&pkts.concat(), |pid| errs.push(pid));
        errs
    }

    #[test]
    fn counting_up_and_wrapping_is_clean() {
        let mut s = RawStats::new();
        let pkts: Vec<_> = (0..40).map(|i| pkt(0x0100, i as u8)).collect();
        assert!(run(&mut s, &pkts).is_empty());
        assert_eq!((s.packets, s.cc_errors, s.distinct_pids), (40, 0, 1));
    }

    #[test]
    fn a_gap_is_an_error_a_duplicate_is_not() {
        let mut s = RawStats::new();
        let errs = run(
            &mut s,
            &[
                pkt(0x0100, 3),
                pkt(0x0100, 3),
                pkt(0x0100, 4),
                pkt(0x0100, 6),
            ],
        );
        assert_eq!(errs, vec![0x0100]);
        assert_eq!(s.cc_errors, 1);
        // the count goes on from the packet after the gap
        assert!(run(&mut s, &[pkt(0x0100, 7)]).is_empty());
        // across buffers: state is kept per PID
        assert_eq!(run(&mut s, &[pkt(0x0200, 0), pkt(0x0100, 9)]), vec![0x0100]);
    }

    #[test]
    fn discontinuity_indicator_restarts_the_count() {
        let mut s = RawStats::new();
        let mut disc = pkt(0x0100, 9);
        disc[3] = 0x30 | 9; // afc 3
        disc[4] = 1;
        disc[5] = 0x80;
        let mut no_flag = disc;
        no_flag[5] = 0x00;
        let mut empty_af = disc;
        empty_af[4] = 0; // a zero-length adaptation field has no flags
        assert!(run(&mut s, &[pkt(0x0100, 1), disc, pkt(0x0100, 10)]).is_empty());
        let mut s = RawStats::new();
        assert_eq!(run(&mut s, &[pkt(0x0100, 1), no_flag]), vec![0x0100]);
        let mut s = RawStats::new();
        assert_eq!(run(&mut s, &[pkt(0x0100, 1), empty_af]), vec![0x0100]);
    }

    #[test]
    fn null_pid_and_payloadless_packets_do_not_count() {
        let mut s = RawStats::new();
        let mut af_only = pkt(0x0100, 9);
        af_only[3] = 0x20 | 9;
        let errs = run(
            &mut s,
            &[
                pkt(0x1FFF, 0),
                pkt(0x1FFF, 7),
                pkt(0x0100, 1),
                af_only,
                pkt(0x0100, 2),
            ],
        );
        assert!(errs.is_empty());
        assert_eq!((s.packets, s.distinct_pids), (5, 2));
    }

    #[test]
    fn tei_link_pid0_sdt_and_raw_pat() {
        let mut s = RawStats::new();
        let mut tei = pkt(0x0100, 0);
        tei[1] |= 0x80;
        let mut link = pkt(0x0100, 1);
        link[3] |= 0x40;
        let pat_cont = pkt(0x0000, 0);
        let mut pat_start = pkt(0x0000, 1);
        pat_start[1] |= 0x40;
        pat_start[100] = 0x42;
        let mut pat_start2 = pkt(0x0000, 2);
        pat_start2[1] |= 0x40;
        let mut lost = pkt(0x0011, 0);
        lost[0] = 0;
        let buf = [
            tei,
            link,
            pat_cont,
            pat_start,
            pat_start2,
            pkt(0x0011, 0),
            lost,
        ]
        .concat();
        let mut with_tail = buf.clone();
        with_tail.extend_from_slice(&pkt(0x0011, 1)[..100]);
        assert_eq!(s.observe(&with_tail, |_| {}), 1);
        assert_eq!(s.packets, 6, "lost-sync and partial packets skipped");
        assert_eq!((s.tei_packets, s.link_encrypted), (1, 1));
        assert_eq!((s.pid0_packets, s.sdt_packets), (3, 1));
        assert_eq!(s.distinct_pids, 3);
        assert_eq!(s.raw_pat, Some(pat_start), "the first PID-0 PUSI packet");
        assert_eq!(s.observe(&buf, |_| {}), 1);
        assert_eq!(s.tei_packets, 2);
        assert_eq!(s.distinct_pids, 3);
    }
}
