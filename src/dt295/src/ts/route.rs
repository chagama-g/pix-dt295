//! The PID routes of an all-services stream.
//!
//! With AUTO_PID off and PROGRAM_NUMBER 0 the device passes the PAT and the PIDs of its
//! route slots, nothing else (hardware 2026-10-29: every service of a TS, and two
//! programmes of one ECM descrambled at once). The router follows the PAT and the PMTs
//! of the delivered TS and works out what the 59 general slots (5-63) carry, most
//! needed first: the SI, the PMTs and ECMs, the PCRs, video / audio, captions, then the
//! data broadcasting. A scrambled PID is bound to the slot of its ECM; an ECM's route
//! names its own slot. NULL is routed too, as in the vendor init: the device hands the
//! TS to USB in 64 KiB blocks, and with the SI alone the first PAT took 4.4 s to come
//! (hardware 2026-10-29); with the stuffing the TS flows at once. Slot 4 is the PAT's (implicit) and the dedicated slots 0-3 stay
//! unused. A slot can be rewritten while the stream runs, so a slot whose PID is no
//! longer needed is reused.
//!
//! Pure logic: the worker writes the [`Op`]s handed out and feeds the decrypted TS.

use super::crc;
use super::packet::{self, PACKET, SYNC};
use super::pat;
use super::pmt::{self, Pmt};
use super::section::SectionBuf;

/// The general route slots.
pub const SLOT_FIRST: u8 = 5;
pub const SLOT_LAST: u8 = 63;
const SLOTS: usize = 64;
/// General slots in all.
pub const CAPACITY: usize = (SLOT_LAST - SLOT_FIRST + 1) as usize;

/// The SI every stream carries: CAT, NIT, SDT, EIT, TDT/TOT and the ARIB tables
/// (SDTT 0x23 / 0x28, BIT, NBIT, the one-seg EIT 0x27, CDT).
pub const SI: [u16; 11] = [
    0x01, 0x10, 0x11, 0x12, 0x14, 0x23, 0x24, 0x25, 0x27, 0x28, 0x29,
];

/// Programs and PIDs one stream keeps track of (a TS has a handful).
const MAX_PROGRAMS: usize = 32;

/// A route slot write: `pid` on `slot`, descrambled with the ECM in slot `ecm_slot`
/// (0: not descrambled; an ECM's route names its own slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Op {
    pub slot: u8,
    pub pid: u16,
    pub ecm_slot: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Route {
    pid: u16,
    ecm_slot: u8,
}

/// The routes of one all-services stream.
#[derive(Debug)]
pub struct Router {
    slots: [Option<Route>; SLOTS],
    /// (program_number, PMT PID) of the last PAT, in its order.
    pat: Vec<(u16, u16)>,
    pmts: Vec<Pmt>,
    /// The sections of PID 0 and the PMT PIDs being reassembled.
    asm: Vec<(u16, SectionBuf)>,
    /// The PSI changed since the last [`Router::plan`].
    changed: bool,
    /// PIDs the last plan had no slot for.
    dropped: usize,
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    /// A router whose first [`Router::plan`] gives the routes a stream starts with (the
    /// SI; the PAT comes on its own).
    pub fn new() -> Self {
        Router {
            slots: [None; SLOTS],
            pat: Vec::new(),
            pmts: Vec::new(),
            asm: vec![(0, SectionBuf::default())],
            changed: true,
            dropped: 0,
        }
    }

    /// Feed delivered TS (decrypted, whole packets): the PAT and the PMTs it lists.
    pub fn observe(&mut self, buf: &[u8]) {
        for p in buf.as_chunks::<PACKET>().0 {
            if p[0] != SYNC || packet::tsc(p) != 0 {
                continue;
            }
            let pid = packet::pid(p);
            let Some((_, a)) = self.asm.iter_mut().find(|(q, _)| *q == pid) else {
                continue;
            };
            // at once: a PAT adds the reassembly of its PMTs, which may follow in `buf`
            for sec in a.push(p) {
                if pid == 0 {
                    self.on_pat(&sec);
                } else {
                    self.on_pmt(pid, &sec);
                }
            }
        }
    }

    fn on_pat(&mut self, sec: &[u8]) {
        if !crc::section_crc_ok(sec) || sec[5] & 1 == 0 {
            return;
        }
        let mut progs = pat::parse_pat(sec);
        progs.truncate(MAX_PROGRAMS);
        if progs == self.pat {
            return;
        }
        self.pmts
            .retain(|m| progs.iter().any(|&(p, _)| p == m.program));
        self.asm
            .retain(|(pid, _)| *pid == 0 || progs.iter().any(|&(_, p)| p == *pid));
        for &(_, pid) in &progs {
            if !self.asm.iter().any(|(p, _)| *p == pid) {
                self.asm.push((pid, SectionBuf::default()));
            }
        }
        self.pat = progs;
        self.changed = true;
    }

    fn on_pmt(&mut self, pid: u16, sec: &[u8]) {
        let Some(pmt) = pmt::parse_pmt(sec) else {
            return;
        };
        if !self.pat.contains(&(pmt.program, pid)) {
            return;
        }
        match self.pmts.iter_mut().find(|m| m.program == pmt.program) {
            Some(m) if *m == pmt => return,
            Some(m) => *m = pmt,
            None => self.pmts.push(pmt),
        }
        self.changed = true;
    }

    /// The PIDs to route, most needed first, at most [`CAPACITY`]: (PID, its ECM PID).
    /// Also counts what did not fit.
    fn wanted(&mut self) -> Vec<(u16, Option<u16>)> {
        // (priority, PID, ECM)
        let mut v: Vec<(u8, u16, Option<u16>)> = SI.iter().map(|&p| (0, p, None)).collect();
        v.extend(self.pat.iter().map(|&(_, pid)| (1, pid, None)));
        for &(prog, _) in &self.pat {
            let Some(m) = self.pmts.iter().find(|m| m.program == prog) else {
                continue;
            };
            let ecms = m.ecm.iter().chain(m.es.iter().filter_map(|e| e.2.as_ref()));
            v.extend(ecms.map(|&e| (1, e, Some(e))));
            v.push((2, m.pcr, None));
            v.extend(
                m.es.iter()
                    .map(|&(pid, st, ecm)| (es_priority(st), pid, ecm)),
            );
        }
        v.retain(|&(_, pid, _)| pid != packet::NULL_PID && pid != 0);
        v.sort_by_key(|&(prio, _, _)| prio); // stable: PAT order within a priority
        let mut out: Vec<(u16, Option<u16>)> = vec![(packet::NULL_PID, None)];
        for (_, pid, ecm) in v {
            match out.iter_mut().find(|(p, _)| *p == pid) {
                // the PCR is often the video PID (CS110: ND06): keep the video's ECM
                Some(e) if e.1.is_none() => e.1 = ecm,
                Some(_) => {}
                None => out.push((pid, ecm)),
            }
        }
        self.dropped = out.len().saturating_sub(CAPACITY);
        out.truncate(CAPACITY);
        out
    }

    /// The slot writes that bring the routes to what the PSI now asks for (empty while
    /// nothing changed). The slots count as written: the caller writes them in order
    /// (an ECM before the PIDs bound to it).
    pub fn plan(&mut self) -> Vec<Op> {
        if !self.changed {
            return Vec::new();
        }
        self.changed = false;
        let want = self.wanted();
        let wanted = |pid: u16| want.iter().any(|&(p, _)| p == pid);
        let mut ops = Vec::new();
        // ECMs first (the PIDs bound to them name their slots), from the top slot down
        for &(pid, _) in want.iter().filter(|&&(p, e)| e == Some(p)) {
            let Some(slot) = self.slot_of(pid).or_else(|| self.free_slot(&wanted, true)) else {
                continue;
            };
            self.put(slot, pid, slot, &mut ops);
        }
        for &(pid, ecm) in want.iter().filter(|&&(p, e)| e != Some(p)) {
            let ecm_slot = ecm.and_then(|e| self.slot_of(e)).unwrap_or(0);
            let Some(slot) = self.slot_of(pid).or_else(|| self.free_slot(&wanted, false)) else {
                continue;
            };
            self.put(slot, pid, ecm_slot, &mut ops);
        }
        // a stale route bound to a slot that is no longer that ECM's: unbind it
        for s in usize::from(SLOT_FIRST)..SLOTS {
            if let Some(r) = self.slots[s] {
                let e = usize::from(r.ecm_slot);
                let is_ecm = |x: &Route| usize::from(x.ecm_slot) == e;
                if e != 0 && e != s && !self.slots[e].as_ref().is_some_and(is_ecm) {
                    self.put(s as u8, r.pid, 0, &mut ops);
                }
            }
        }
        ops
    }

    /// Set a slot (an op only if it changes).
    fn put(&mut self, slot: u8, pid: u16, ecm_slot: u8, ops: &mut Vec<Op>) {
        let r = Some(Route { pid, ecm_slot });
        if self.slots[usize::from(slot)] != r {
            self.slots[usize::from(slot)] = r;
            ops.push(Op {
                slot,
                pid,
                ecm_slot,
            });
        }
    }

    fn slot_of(&self, pid: u16) -> Option<u8> {
        (SLOT_FIRST..=SLOT_LAST).find(|&s| self.slots[usize::from(s)].is_some_and(|r| r.pid == pid))
    }

    /// An empty slot, else one whose PID is not wanted (ECMs from the top, the rest from
    /// the bottom; a stale ECM's slot last, PIDs may still be bound to it).
    fn free_slot(&self, wanted: &dyn Fn(u16) -> bool, top: bool) -> Option<u8> {
        let mut order: Vec<u8> = (SLOT_FIRST..=SLOT_LAST).collect();
        if top {
            order.reverse();
        }
        let at = |s: u8| self.slots[usize::from(s)];
        let stale = |ecm: bool| {
            order
                .iter()
                .copied()
                .find(|&s| at(s).is_some_and(|r| !wanted(r.pid) && (r.ecm_slot == s) == ecm))
        };
        order
            .iter()
            .copied()
            .find(|&s| at(s).is_none())
            .or_else(|| stale(false))
            .or_else(|| stale(true))
    }

    /// The video / audio PIDs routed with an ECM (what a card session descrambles; the
    /// one-seg service is not scrambled, so it tells nothing about the card).
    pub fn scrambled_av(&self) -> Vec<u16> {
        let mut v: Vec<u16> = Vec::new();
        for m in &self.pmts {
            for &(pid, st, ecm) in &m.es {
                if pmt::is_av(st)
                    && ecm.is_some()
                    && self.slot_of(pid).is_some()
                    && !v.contains(&pid)
                {
                    v.push(pid);
                }
            }
        }
        v
    }

    /// For the log: programs, PMTs read, routes in use and ECMs.
    pub fn summary(&self) -> String {
        let used = self.slots.iter().filter(|r| r.is_some()).count();
        let mut ecms: Vec<String> = Vec::new();
        for (s, r) in self.slots.iter().enumerate() {
            if let Some(r) = r.filter(|r| usize::from(r.ecm_slot) == s) {
                ecms.push(format!("0x{:04x}", r.pid));
            }
        }
        format!(
            "{} program(s), {} PMT(s) read, {used} of {CAPACITY} route slots, ECM [{}]",
            self.pat.len(),
            self.pmts.len(),
            ecms.join(" ")
        )
    }

    /// PIDs the last plan had no slot for.
    pub fn dropped(&self) -> usize {
        self.dropped
    }
}

/// Video / audio first, then captions and superimposed text (PES private data), the
/// data broadcasting (DSM-CC) and the rest.
fn es_priority(stream_type: u8) -> u8 {
    match stream_type {
        st if pmt::is_av(st) => 3,
        0x06 => 4,
        0x0D => 5,
        _ => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::pmt::{TAG_CA, TID_PMT, parse_pmt};
    use crate::ts::testutil::{make_pat_packet, packetize};

    /// A PMT section: program-level ECM `ecm`, `streams` = (stream_type, PID, ES-level
    /// CA PID).
    fn pmt(
        program: u16,
        pcr: u16,
        ecm: Option<u16>,
        streams: &[(u8, u16, Option<u16>)],
    ) -> Vec<u8> {
        let ca = |pid: u16| [TAG_CA, 4, 0x00, 0x05, 0xE0 | (pid >> 8) as u8, pid as u8];
        let info: Vec<u8> = ecm.map_or_default(|e| ca(e).to_vec());
        let mut s = vec![TID_PMT, 0, 0];
        s.extend_from_slice(&program.to_be_bytes());
        s.extend_from_slice(&[0xC1, 0, 0, 0xE0 | (pcr >> 8) as u8, pcr as u8]);
        s.extend_from_slice(&[0xF0 | (info.len() >> 8) as u8, info.len() as u8]);
        s.extend_from_slice(&info);
        for &(st, pid, own) in streams {
            let d: Vec<u8> = own.map_or_default(|e| ca(e).to_vec());
            s.extend_from_slice(&[st, 0xE0 | (pid >> 8) as u8, pid as u8]);
            s.extend_from_slice(&[0xF0 | (d.len() >> 8) as u8, d.len() as u8]);
            s.extend_from_slice(&d);
        }
        let len = s.len() - 3 + 4;
        s[1] = 0xB0 | (len >> 8) as u8;
        s[2] = len as u8;
        let c = crc::crc32(&s);
        s.extend_from_slice(&c.to_be_bytes());
        s
    }

    /// Tokyo MX on 20ch (hardware 2026-10-29): MX1 and MX2 with their own video /
    /// audio under one ECM, and the one-seg service.
    fn mx() -> Vec<u8> {
        let mut b = make_pat_packet(
            &[(0, 0x10), (23608, 257), (23610, 259), (23992, 8136)],
            0,
            false,
        )
        .to_vec();
        let mut cc = 0;
        let mx1 = pmt(
            23608,
            256,
            Some(49),
            &[
                (0x02, 273, None),
                (0x0F, 274, None),
                (0x0D, 1856, Some(0x1FFF)),
            ],
        );
        let mx2 = pmt(
            23610,
            256,
            Some(49),
            &[(0x02, 305, None), (0x0F, 306, None), (0x06, 308, None)],
        );
        let seg = pmt(23992, 512, None, &[(0x1B, 641, None), (0x0F, 643, None)]);
        b.extend(packetize(257, &mx1, &mut cc, 0));
        b.extend(packetize(259, &mx2, &mut cc, 0));
        b.extend(packetize(8136, &seg, &mut cc, 0));
        b
    }

    fn routes(r: &Router) -> Vec<(u8, u16, u8)> {
        (0..SLOTS as u8)
            .filter_map(|s| r.slots[usize::from(s)].map(|x| (s, x.pid, x.ecm_slot)))
            .collect()
    }

    fn slot(r: &Router, pid: u16) -> Route {
        r.slots[usize::from(r.slot_of(pid).unwrap())].unwrap()
    }

    #[test]
    fn starts_with_the_si() {
        let mut r = Router::new();
        let ops = r.plan();
        assert_eq!(ops.len(), 1 + SI.len());
        assert_eq!(
            ops[0],
            Op {
                slot: 5,
                pid: packet::NULL_PID,
                ecm_slot: 0
            }
        );
        assert_eq!(
            ops[1],
            Op {
                slot: 6,
                pid: 0x01,
                ecm_slot: 0
            }
        );
        assert!(r.plan().is_empty());
    }

    #[test]
    fn pat_then_pmts_then_es() {
        let mut r = Router::new();
        r.plan();
        let b = mx();
        r.observe(&b[..PACKET]); // the PAT only: its PMTs are not routed yet
        let ops = r.plan();
        assert_eq!(
            ops.iter().map(|o| o.pid).collect::<Vec<_>>(),
            [257, 259, 8136]
        );
        r.observe(&b);
        let ops = r.plan();
        // the ECM first, in the top slot, bound to itself
        assert_eq!(
            ops[0],
            Op {
                slot: 63,
                pid: 49,
                ecm_slot: 63
            }
        );
        assert_eq!(slot(&r, 273).ecm_slot, 63);
        assert_eq!(slot(&r, 305).ecm_slot, 63);
        assert_eq!(slot(&r, 308).ecm_slot, 63);
        assert_eq!(slot(&r, 1856).ecm_slot, 0, "explicitly not scrambled");
        assert_eq!(slot(&r, 641).ecm_slot, 0, "one-seg: no ECM");
        assert!(r.slot_of(256).is_some() && r.slot_of(512).is_some(), "PCRs");
        assert_eq!(r.scrambled_av(), [273, 274, 305, 306]);
        assert!(r.plan().is_empty(), "nothing changed");
        r.observe(&b);
        assert!(r.plan().is_empty(), "the same PSI again");
    }

    #[test]
    fn a_new_pmt_version_adds_only_the_new_pid() {
        let mut r = Router::new();
        r.plan();
        r.observe(&mx());
        r.plan();
        let before = routes(&r);
        let mx2 = pmt(
            23610,
            256,
            Some(49),
            &[(0x02, 305, None), (0x0F, 306, None), (0x0F, 307, None)],
        );
        r.observe(&packetize(259, &mx2, &mut 5, 0));
        let ops = r.plan();
        assert_eq!(ops.len(), 1);
        assert_eq!((ops[0].pid, ops[0].ecm_slot), (307, 63));
        // the slot 308 had is kept (unwanted now, reused only when the slots run out)
        assert_eq!(routes(&r).len(), before.len() + 1);
    }

    #[test]
    fn stale_slots_are_reused_when_full() {
        let mut r = Router::new();
        r.plan();
        // fill every slot: NULL, the SI, the PMT, the PCR and 45 ES of one program
        let many: Vec<(u8, u16, Option<u16>)> = (0..45).map(|i| (0x0D, 0x200 + i, None)).collect();
        let mut b = make_pat_packet(&[(0, 0x10), (1, 0x100)], 0, false).to_vec();
        let mut cc = 0;
        b.extend(packetize(0x100, &pmt(1, 0x1FF, None, &many), &mut cc, 0));
        r.observe(&b);
        r.plan();
        assert_eq!(routes(&r).len(), CAPACITY);
        assert_eq!(r.dropped(), 0);
        // the next version replaces the data PIDs with a video and two more data PIDs
        let mut next: Vec<(u8, u16, Option<u16>)> = vec![(0x02, 0x111, None)];
        next.extend((0..49).map(|i| (0x0D, 0x300 + i, None)));
        r.observe(&packetize(0x100, &pmt(1, 0x1FF, None, &next), &mut cc, 0));
        r.plan();
        assert!(r.slot_of(0x111).is_some(), "video before data");
        assert_eq!(routes(&r).len(), CAPACITY);
        assert_eq!(r.dropped(), 5);
        assert!(r.slot_of(0x200).is_none(), "stale data PIDs gave way");
    }

    #[test]
    fn a_new_ecm_gets_its_own_slot() {
        let mut r = Router::new();
        r.plan();
        let mut b = make_pat_packet(&[(0, 0x10), (1, 0x100)], 0, false).to_vec();
        b.extend(packetize(
            0x100,
            &pmt(1, 0x1FF, Some(0x901), &[(0x02, 0x111, None)]),
            &mut 0,
            0,
        ));
        r.observe(&b);
        r.plan();
        assert_eq!(slot(&r, 0x111).ecm_slot, 63);
        // a new ECM PID; the old ECM and video are no longer wanted
        let next = pmt(1, 0x1FF, Some(0x902), &[(0x02, 0x112, None)]);
        r.observe(&packetize(0x100, &next, &mut 1, 0));
        r.plan();
        assert_eq!(slot(&r, 0x902).ecm_slot, r.slot_of(0x902).unwrap());
        assert_eq!(slot(&r, 0x112).ecm_slot, r.slot_of(0x902).unwrap());
        // the stale video stays bound to the stale ECM, which kept its slot
        assert_eq!(slot(&r, 0x111).ecm_slot, r.slot_of(0x901).unwrap());
    }

    #[test]
    fn a_program_gone_from_the_pat_is_forgotten() {
        let mut r = Router::new();
        r.plan();
        r.observe(&mx());
        r.plan();
        let mut b = make_pat_packet(&[(0, 0x10), (23608, 257)], 0, false).to_vec();
        b[3] = 0x11; // the next continuity counter
        r.observe(&b);
        r.plan();
        assert_eq!(r.pmts.len(), 1);
        assert_eq!(r.scrambled_av(), [273, 274]);
    }

    #[test]
    fn bad_crc_and_wrong_pid_are_ignored() {
        let mut r = Router::new();
        r.plan();
        r.observe(&make_pat_packet(&[(0, 0x10), (1, 0x100)], 0, true));
        assert!(r.plan().is_empty());
        r.observe(&make_pat_packet(&[(0, 0x10), (1, 0x100)], 0, false));
        r.plan();
        // a PMT of program 1 on a PID the PAT does not give it
        r.observe(&packetize(
            0x101,
            &pmt(1, 0x1FF, None, &[(0x02, 0x111, None)]),
            &mut 0,
            0,
        ));
        assert!(r.plan().is_empty());
    }

    #[test]
    fn a_section_split_over_packets_and_reads() {
        let mut r = Router::new();
        r.plan();
        let mut b = make_pat_packet(&[(0, 0x10), (1, 0x100)], 0, false).to_vec();
        r.observe(&b);
        r.plan();
        let es: Vec<(u8, u16, Option<u16>)> = (0..60).map(|i| (0x0D, 0x200 + i, None)).collect();
        b = packetize(0x100, &pmt(1, 0x1FF, None, &es), &mut 0, 7);
        assert!(b.len() > PACKET, "spans packets");
        r.observe(&b[..PACKET]);
        assert!(r.plan().is_empty());
        r.observe(&b[PACKET..]);
        assert_eq!(r.pmts.len(), 1);
    }

    #[test]
    fn a_pcr_on_the_video_pid_keeps_its_ecm() {
        // CS110 ND06 (hardware 2026-10-30): the PCR is carried on the scrambled video
        let mut r = Router::new();
        r.plan();
        let mut b = make_pat_packet(&[(0, 0x10), (294, 0x404)], 0, false).to_vec();
        let sec = pmt(
            294,
            0x1004,
            Some(0x604),
            &[(0x02, 0x1004, None), (0x0F, 0x1044, None)],
        );
        b.extend(packetize(0x404, &sec, &mut 0, 0));
        r.observe(&b);
        r.plan();
        let ecm = r.slot_of(0x604).unwrap();
        assert_eq!(
            slot(&r, 0x1004).ecm_slot,
            ecm,
            "video = PCR, bound to the ECM"
        );
        assert_eq!(slot(&r, 0x1044).ecm_slot, ecm);
        assert_eq!(r.scrambled_av(), [0x1004, 0x1044]);
    }

    #[test]
    fn es_level_ca() {
        let sec = pmt(
            1,
            0x1FF,
            Some(0x901),
            &[
                (0x02, 0x111, None),
                (0x06, 0x130, Some(0x1FFF)),
                (0x0F, 0x112, Some(0x902)),
            ],
        );
        let m = parse_pmt(&sec).unwrap();
        assert_eq!(m.ecm, Some(0x901));
        assert_eq!(
            m.es,
            [
                (0x111, 0x02, Some(0x901)),
                (0x130, 0x06, None),
                (0x112, 0x0F, Some(0x902))
            ]
        );
    }
}
