//! Whether the stream's video / audio come out descrambled.
//!
//! Without a B-CAS card session the device keeps streaming the program without its
//! video and audio (hardware 2026-10-28: the rate dropped by the video's ~15 Mbit/s,
//! TVTest counted no scrambled packet and showed no picture, and only the unscrambled
//! captions came through). So: the video / audio PIDs of the program's PMT must carry
//! PES packets (unit starts beginning with the start code 00 00 01); none in a window
//! means the card session is missing.

use super::packet::{self, PACKET};
use super::{pat, pmt};

/// Video / audio PIDs one program keeps.
const MAX_AV: usize = 8;

/// The unit starts of one judging window (the worker takes a verdict every 2 s).
#[derive(Debug, Default)]
pub struct EsCheck {
    /// The program judged (the single-service output); `None`: nothing to judge, or an
    /// all-services stream ([`EsCheck::observe_av`]).
    program: Option<u16>,
    pmt_pid: Option<u16>,
    /// The video / audio PIDs its PMT lists (empty until a PMT was read).
    av: Vec<u16>,
    /// Read the PMT again (once per window: it may change).
    read_pmt: bool,
    /// Packets and unit starts on those PIDs in this window, and how many unit starts
    /// began a PES packet.
    packets: u32,
    starts: u32,
    pes: u32,
    /// The last window's counts, for the log ([`EsCheck::last_window`]).
    last: (u32, u32, u32),
}

impl EsCheck {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count the whole packets of `buf` (decrypted, as the device sent them) of the
    /// single-service `program` (`None`: PSI only, nothing to judge).
    pub fn observe(&mut self, buf: &[u8], program: Option<u16>) {
        if program != self.program {
            *self = Self {
                program,
                read_pmt: true,
                ..Self::default()
            };
        }
        if self.program.is_none() {
            return;
        }
        if self.pmt_pid.is_none() {
            let prog = self.program;
            self.pmt_pid = pat::find_section(buf, 0).and_then(|sec| {
                pat::parse_pat(&sec)
                    .into_iter()
                    .find(|&(p, _)| Some(p) == prog)
                    .map(|(_, pid)| pid)
            });
        }
        if let (Some(pid), true) = (self.pmt_pid, self.read_pmt)
            && let Some(av) = pat::find_section(buf, pid).and_then(|sec| av_pids(&sec))
        {
            self.av = av;
            self.read_pmt = false;
        }
        self.count(buf);
    }

    /// Count the whole packets of `buf` on `av`: the scrambled video / audio PIDs of an
    /// all-services stream (the PAT / PMTs are the router's).
    pub fn observe_av(&mut self, buf: &[u8], av: &[u16]) {
        if self.av != av {
            self.av = av.to_vec();
        }
        self.count(buf);
    }

    fn count(&mut self, buf: &[u8]) {
        if self.av.is_empty() {
            return;
        }
        for p in buf.as_chunks::<PACKET>().0 {
            if !self.av.contains(&packet::pid(p)) {
                continue;
            }
            self.packets += 1;
            if !packet::pusi(p) {
                continue;
            }
            let Some(off) = packet::payload_offset(p) else {
                continue;
            };
            self.starts += 1;
            if p[off..].starts_with(&[0, 0, 1]) {
                self.pes += 1;
            }
        }
    }

    /// The window's verdict, and a new window: `Some(true)` the video / audio carry PES
    /// packets, `Some(false)` none came (missing, or no readable PES header): no card
    /// session. `None`: no PMT read yet, or one without video / audio (data
    /// broadcasting), or PSI only.
    pub fn verdict(&mut self) -> Option<bool> {
        let v = (!self.av.is_empty()).then_some(self.pes > 0);
        self.last = (self.packets, self.starts, self.pes);
        self.packets = 0;
        self.starts = 0;
        self.pes = 0;
        self.read_pmt = true;
        v
    }
}

impl EsCheck {
    /// What the last verdict was taken on, for the log: the PMT PID, the video / audio
    /// PIDs, and that window's packets, unit starts and PES starts on them.
    pub fn last_window(&self) -> String {
        let pids: Vec<String> = self.av.iter().map(|p| format!("0x{p:04x}")).collect();
        let (packets, starts, pes) = self.last;
        format!(
            "PMT {}, video/audio PIDs [{}]: {packets} packet(s), {starts} unit start(s), {pes}              PES",
            self.pmt_pid.map_or("-".into(), |p| format!("0x{p:04x}")),
            pids.join(" ")
        )
    }
}

/// The video / audio PIDs of a CRC-valid PMT section (`None`: not a PMT).
fn av_pids(sec: &[u8]) -> Option<Vec<u16>> {
    let pmt = pmt::parse_pmt(sec)?;
    let av = pmt.es.iter().filter(|e| pmt::is_av(e.1)).map(|e| e.0);
    Some(av.take(MAX_AV).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::crc;
    use crate::ts::packet::SYNC;
    use crate::ts::pmt::TID_PMT;
    use crate::ts::testutil::{make_pat_packet, packetize};

    const PMT_PID: u16 = 0x01F0;

    /// A PMT section of program 1024: `streams` = (stream_type, PID).
    fn pmt_section(streams: &[(u8, u16)]) -> Vec<u8> {
        let mut s = vec![
            TID_PMT, 0, 0, 0x04, 0x00, 0xC1, 0, 0, 0xE1, 0x00, 0xF0, 0x00,
        ];
        for &(st, pid) in streams {
            s.extend_from_slice(&[st, 0xE0 | (pid >> 8) as u8, pid as u8, 0xF0, 0x00]);
        }
        let len = s.len() - 3 + 4;
        s[1] = 0xB0 | (len >> 8) as u8;
        s[2] = len as u8;
        let c = crc::crc32(&s);
        s.extend_from_slice(&c.to_be_bytes());
        s
    }

    /// The PSI of program 1024 (PAT + PMT with video 0x0111 and audio 0x0112).
    fn psi() -> Vec<u8> {
        let mut b = make_pat_packet(&[(0, 0x10), (1024, PMT_PID)], 0, false).to_vec();
        b.extend(packetize(
            PMT_PID,
            &pmt_section(&[(0x02, 0x111), (0x0F, 0x112), (0x06, 0x130)]),
            &mut 0,
            0,
        ));
        b
    }

    /// A unit start on `pid` whose payload begins with `head`.
    fn start(pid: u16, head: &[u8]) -> [u8; PACKET] {
        let mut p = [0x5Au8; PACKET];
        p[..4].copy_from_slice(&[SYNC, 0x40 | (pid >> 8) as u8, pid as u8, 0x10]);
        p[4..4 + head.len()].copy_from_slice(head);
        p
    }

    #[test]
    fn video_and_audio_pes_are_clear() {
        let mut chk = EsCheck::new();
        let mut b = psi();
        b.extend(start(0x111, &[0, 0, 1, 0xE0]));
        chk.observe(&b, Some(1024));
        assert_eq!(chk.verdict(), Some(true));
    }

    #[test]
    fn missing_or_garbled_av_is_scrambled() {
        let mut chk = EsCheck::new();
        let mut b = psi();
        b.extend(start(0x130, &[0, 0, 1, 0xBD])); // captions only
        chk.observe(&b, Some(1024));
        assert_eq!(chk.verdict(), Some(false));
        // the PMT stays known: a window of garbled video unit starts
        chk.observe(&start(0x111, &[0x9C, 0x31, 0x07]), Some(1024));
        assert_eq!(chk.verdict(), Some(false));
        chk.observe(&start(0x112, &[0, 0, 1, 0xC0]), Some(1024));
        assert_eq!(chk.verdict(), Some(true));
    }

    #[test]
    fn all_services_judge_the_given_pids() {
        let mut chk = EsCheck::new();
        chk.observe_av(&start(0x305, &[0, 0, 1, 0xE0]), &[0x111, 0x305]);
        assert_eq!(chk.verdict(), Some(true), "another programme's video");
        chk.observe_av(&start(0x641, &[0, 0, 1, 0xE0]), &[0x111, 0x305]);
        assert_eq!(chk.verdict(), Some(false), "only the one-seg video");
        chk.observe_av(&start(0x111, &[0, 0, 1, 0xE0]), &[]);
        assert_eq!(chk.verdict(), None, "nothing scrambled to judge");
    }

    #[test]
    fn no_verdict_without_pmt_or_program() {
        let mut chk = EsCheck::new();
        chk.observe(&start(0x111, &[0, 0, 1, 0xE0]), Some(1024)); // no PSI yet
        assert_eq!(chk.verdict(), None);
        chk.observe(&psi(), None); // PSI only
        assert_eq!(chk.verdict(), None);
        // another program's PAT entry only
        let mut b = make_pat_packet(&[(1025, 0x3F0)], 0, false).to_vec();
        b.extend(start(0x111, &[0, 0, 1, 0xE0]));
        chk.observe(&b, Some(1024));
        assert_eq!(chk.verdict(), None);
    }

    #[test]
    fn data_broadcasting_has_no_verdict() {
        let mut chk = EsCheck::new();
        let mut b = make_pat_packet(&[(1024, PMT_PID)], 0, false).to_vec();
        b.extend(packetize(
            PMT_PID,
            &pmt_section(&[(0x0D, 0x140)]),
            &mut 0,
            0,
        ));
        chk.observe(&b, Some(1024));
        assert_eq!(chk.verdict(), None);
    }

    #[test]
    fn a_new_program_starts_over() {
        let mut chk = EsCheck::new();
        chk.observe(&psi(), Some(1024));
        assert_eq!(chk.verdict(), Some(false));
        chk.observe(&start(0x111, &[0, 0, 1, 0xE0]), Some(1025)); // its PMT unknown
        assert_eq!(chk.verdict(), None);
    }

    #[test]
    fn pmt_streams() {
        let sec = pmt_section(&[(0x02, 0x111), (0x0F, 0x112), (0x06, 0x130), (0x0D, 0x140)]);
        assert_eq!(av_pids(&sec), Some(vec![0x111, 0x112]));
        let mut bad = sec.clone();
        bad[13] ^= 1;
        assert_eq!(av_pids(&bad), None); // CRC
        assert_eq!(av_pids(&[0u8; 20]), None);
    }
}
