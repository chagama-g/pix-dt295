//! The satellite PSI keeper.
//!
//! On a BS/CS pay TS the card has no contract for, the device delivers little or no
//! TS, often not even PSI, so a host scan cannot list its services. The keeper caches
//! the PAT, PMT, NIT\[actual\] and SDT\[actual/other\] sections it sees and, while the
//! tuned satellite TS is starved of a table the device has not delivered in this
//! stream, re-sends the cached copies (a PAT is built from the SDT service list when
//! none was ever seen; a PMT is never made up -- TVTest drops a service whose PMT lists
//! no playable video). On a satellite stream with a tuned service it also owns PID
//! 0x0000 / 0x0011: the device outputs one service but announces every service of the
//! TS, so its packets are blanked and their sections re-sent with the PAT reduced to
//! NIT + the tuned program and the SDT\[actual\] reduced to the tuned service.
//! Terrestrial streams pass untouched.
//!
//! Why: what the device delivers on such a TS differs from one tune to the next
//! (hardware 2026-10-18, two BS scans minutes apart: one channel gave 18 PATs + 6 SDT
//! packets in one and 3 PATs + no SDT in the other, another 0 bytes in both), so each
//! host scan registered a different subset of the pay channels. The PSI itself is
//! ordinary broadcast data, delivered on other occasions (and as SDT\[other\] on other
//! TSes). A healthy stream carries its own PAT ~10x/s and its own SDT and gets nothing
//! added.

use std::fmt;
use std::time::{Duration, Instant};

use super::crc;
use super::packet::{self, NULL_PID, PACKET, SYNC};
use super::pat;
use crate::{PSI_KEEP_DAYS, PsiRecord};

/// PSI sections are at most 1024 bytes.
const SEC_MAX: usize = 1024;
/// Cached sections at most.
const MAX_ENT: usize = 640;
/// Section reassemblers per stream.
const MAX_ASM: usize = 40;
/// PMT PIDs watched per stream.
const MAX_PMT: usize = 32;
/// SDT sections of one TS sent / used at most.
const MAX_SDT_SET: usize = 16;
/// PMT PIDs of a synthesized PAT: 0x1F00 + index.
const SYN_PMT: u16 = 0x1F00;

const PAT_PID: u16 = 0x0000;
const NIT_PID: u16 = 0x0010;
const SDT_PID: u16 = 0x0011;

const TID_PAT: u8 = 0x00;
const TID_PMT: u8 = 0x02;
const TID_NIT: u8 = 0x40;
const TID_SDT: u8 = 0x42;
const TID_SDT_OTHER: u8 = 0x46;

/// No continuity_counter known for a PID.
const CC_NONE: u8 = 0xFF;

/// The device's chance first: nothing is re-sent in the first second of a stream
/// (unless the TS is known silent -- TVTest's scan looks ~2 s after SetChannel and,
/// once the SDT is there, decides).
const GRACE: Duration = Duration::from_millis(1000);
/// A delivering stream without a PAT gets one after this long ...
const PAT_AFTER: Duration = Duration::from_millis(1500);
/// ... and again every this long (every [`PAT_EVERY_SILENT`] while nothing at all came).
const PAT_EVERY: Duration = Duration::from_millis(500);
const PAT_EVERY_SILENT: Duration = Duration::from_millis(400);
/// The same for the SDT.
const SDT_AFTER: Duration = Duration::from_millis(3000);
const SDT_EVERY: Duration = Duration::from_millis(1000);
const SDT_EVERY_SILENT: Duration = Duration::from_millis(500);
/// The NIT (only while the PAT is missing too).
const NIT_AFTER: Duration = Duration::from_millis(5000);
const NIT_EVERY: Duration = Duration::from_millis(2000);

/// What the worker knows about the running stream, per call.
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamView {
    /// The service being streamed, if one.
    pub program: Option<u16>,
    /// The device has delivered at least one byte in this stream.
    pub delivered_any: bool,
    /// The link key is known not to decrypt this stream: add nothing (such a stream
    /// is not starved of PSI, its PSI is just unreadable).
    pub undecryptable: bool,
}

/// What a cached section is ([`PsiRecord::kind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Kind {
    Pat = 1,
    Pmt = 2,
    Nit = 3,
    Sdt = 4,
}

impl Kind {
    fn from_u8(k: u8) -> Option<Self> {
        match k {
            1 => Some(Self::Pat),
            2 => Some(Self::Pmt),
            3 => Some(Self::Nit),
            4 => Some(Self::Sdt),
            _ => None,
        }
    }
}

/// What identifies a cached section (one entry per key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    kind: Kind,
    /// PAT/PMT/SDT: transport_stream_id; NIT: network_id.
    id: u16,
    /// PMT: program_number; NIT/SDT: section_number.
    sub: u16,
}

impl Key {
    fn new(kind: Kind, id: u16, sub: u16) -> Self {
        Self { kind, id, sub }
    }
}

/// One cached section.
#[derive(Debug, Clone)]
struct Entry {
    key: Key,
    /// An SDT kept from SDT\[other\] (table 0x46).
    other: bool,
    /// Day (Unix time / 86400) the device last delivered it.
    seen: u32,
    /// table_id .. CRC_32.
    sec: Vec<u8>,
}

impl Entry {
    /// A section of table `kind` / `id` (any section number).
    fn is(&self, kind: Kind, id: u16) -> bool {
        self.key.kind == kind && self.key.id == id
    }
}

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

/// The service loop of an SDT section: (offset, service_id, descriptors_loop_length)
/// of each entry whose 5-byte header lies before the CRC. The descriptors themselves
/// may run past it (callers bound them).
fn sdt_services(sec: &[u8]) -> impl Iterator<Item = (usize, u16, usize)> + '_ {
    let end = sec.len().saturating_sub(4);
    let mut o = 11;
    std::iter::from_fn(move || {
        if o + 5 > end {
            return None;
        }
        let at = o;
        let dl = usize::from(be16(sec, o + 3) & 0x0FFF);
        o += 5 + dl;
        Some((at, be16(sec, at), dl))
    })
}

/// The 8-byte long-form header of section 0 of 0; [`finish`] completes it.
fn long_header(tid: u8, id: u16, version: u8) -> Vec<u8> {
    let mut sec = Vec::with_capacity(SEC_MAX);
    let [hi, lo] = id.to_be_bytes();
    sec.extend_from_slice(&[tid, 0, 0, hi, lo, 0xC1 | ((version & 0x1F) << 1), 0, 0]);
    sec
}

/// Append the CRC_32 and set section_length (syntax indicator set, reserved bits 1).
fn finish(sec: &mut Vec<u8>) {
    sec.extend_from_slice(&[0; 4]);
    let sl = sec.len() - 3;
    sec[1] = 0xB0 | ((sl >> 8) as u8 & 0x0F);
    sec[2] = sl as u8;
    crc::set_section_crc(sec);
}

/// The section cache.
#[derive(Debug, Default)]
struct Cache {
    ent: Vec<Entry>,
    /// Added or refreshed since the last export / import.
    dirty: bool,
    full_logged: bool,
}

impl Cache {
    fn find(&self, key: Key) -> Option<&Entry> {
        self.ent.iter().find(|e| e.key == key)
    }

    /// Keep one section (`last` = its last_section_number). An SDT\[other\] copy never
    /// displaces an SDT\[actual\] one. A NIT / SDT that shrank loses its higher
    /// sections. An unchanged section only refreshes its `seen` day.
    fn store(&mut self, key: Key, other: bool, last: u16, sec: &[u8], seen: u32) {
        // A section numbered past its own last_section_number is malformed: ignore it.
        if matches!(key.kind, Kind::Nit | Kind::Sdt) && key.sub > last {
            return;
        }
        let mut found = None;
        if let Some(i) = self.ent.iter().position(|e| e.key == key) {
            let e = &mut self.ent[i];
            if other && !e.other {
                return;
            }
            if e.other == other && e.sec == sec {
                if seen > e.seen {
                    e.seen = seen; // still on air
                    self.dirty = true;
                }
                return;
            }
            found = Some(i);
        }
        if matches!(key.kind, Kind::Nit | Kind::Sdt) {
            for i in (0..self.ent.len()).rev() {
                let e = &self.ent[i];
                if e.is(key.kind, key.id) && e.key.sub > last && (!other || e.other) {
                    let moved = self.ent.len() - 1; // swap_remove moves the last entry into i
                    self.ent.swap_remove(i);
                    self.dirty = true;
                    found = match found {
                        Some(f) if f == i => None, // the old copy itself went
                        Some(f) if f == moved => Some(i),
                        f => f,
                    };
                }
            }
        }
        let entry = Entry {
            key,
            other,
            seen,
            sec: sec.to_vec(),
        };
        match found {
            Some(f) => self.ent[f] = entry,
            None if self.ent.len() >= MAX_ENT => {
                if !self.full_logged {
                    self.full_logged = true;
                    tracing::warn!("psi: section cache full");
                }
                return;
            }
            None => self.ent.push(entry),
        }
        self.dirty = true;
    }

    /// Whether a cached SDT\[actual\] of `tsid` lists `sid`.
    fn sdt_lists(&self, tsid: u16, sid: u16) -> bool {
        self.ent
            .iter()
            .filter(|e| e.is(Kind::Sdt, tsid) && !e.other)
            .any(|e| sdt_services(&e.sec).any(|(_, s, _)| s == sid))
    }

    /// The SDT sections of `tsid` in section order: \[actual\] if any was seen, else
    /// \[other\].
    fn sdt_set(&self, tsid: u16, max: usize) -> Vec<&Entry> {
        let of_ts = |e: &&Entry| e.is(Kind::Sdt, tsid);
        let actual = self.ent.iter().filter(of_ts).any(|e| !e.other);
        let mut set: Vec<&Entry> = self
            .ent
            .iter()
            .filter(of_ts)
            .filter(|e| e.other != actual && e.key.sub < 256)
            .collect();
        set.sort_by_key(|e| e.key.sub); // stable: the first entry of a section number wins
        set.dedup_by_key(|e| e.key.sub);
        set.truncate(max);
        set
    }

    /// The PAT / SDT\[actual\] of `tsid` reduced to service `sid`; `None` = send it
    /// unchanged (another table, or a stale sid the table does not list -- never
    /// announce nothing).
    fn filter(&self, tsid: u16, sid: u16, sec: &[u8]) -> Option<Vec<u8>> {
        if sec.len() < 12 {
            return None;
        }
        let end = sec.len() - 4;
        let mut out;
        if sec[0] == TID_PAT {
            out = sec[..8].to_vec();
            let mut keep = false;
            for e in sec[8..end].as_chunks::<4>().0 {
                let prog = be16(e, 0);
                if prog == 0 || prog == sid {
                    out.extend_from_slice(e);
                    keep |= prog == sid;
                }
            }
            if !keep {
                return None;
            }
        } else if sec[0] == TID_SDT && be16(sec, 3) == tsid {
            if !self.sdt_lists(tsid, sid) {
                return None;
            }
            // a section without the service goes out with an empty loop
            out = sec[..11].to_vec();
            for (o, s, dl) in sdt_services(sec) {
                if s == sid && o + 5 + dl <= end {
                    out.extend_from_slice(&sec[o..o + 5 + dl]);
                }
            }
        } else {
            return None;
        }
        // keep the high nibble of byte 1 (an SDT has reserved_future_use set)
        out.extend_from_slice(&[0; 4]);
        let sl = out.len() - 3;
        out[1] = (sec[1] & 0xF0) | ((sl >> 8) as u8 & 0x0F);
        out[2] = sl as u8;
        crc::set_section_crc(&mut out);
        Some(out)
    }
}

/// The last continuity_counter per PID (the device's, or ours once we sent).
struct CcTable(Box<[u8; 8192]>);

impl CcTable {
    fn new() -> Self {
        Self(Box::new([CC_NONE; 8192]))
    }

    fn reset(&mut self) {
        self.0.fill(CC_NONE);
    }

    fn set(&mut self, pid: u16, cc: u8) {
        self.0[usize::from(pid & 0x1FFF)] = cc;
    }

    /// Send one section on `pid`, continuing that PID's continuity_counter.
    fn emit(&mut self, pid: u16, sec: &[u8], out: &mut Vec<u8>) {
        let pid = pid & 0x1FFF;
        let [pid_hi, pid_lo] = pid.to_be_bytes();
        let slot = &mut self.0[usize::from(pid)];
        let mut off = 0;
        while off < sec.len() {
            let first = off == 0;
            let n = (sec.len() - off).min(PACKET - if first { 5 } else { 4 });
            let cc = if *slot == CC_NONE {
                0
            } else {
                (*slot + 1) & 0x0F
            };
            *slot = cc;
            let pusi = if first { 0x40 } else { 0x00 };
            let start = out.len();
            out.extend_from_slice(&[SYNC, pusi | pid_hi, pid_lo, 0x10 | cc]);
            if first {
                out.push(0x00); // pointer_field
            }
            out.extend_from_slice(&sec[off..off + n]);
            out.resize(start + PACKET, 0xFF);
            off += n;
        }
    }
}

/// Send a PSI section; on an owned stream (`own` = the tuned service) the PAT /
/// SDT\[actual\] reduced first.
fn send(
    cache: &Cache,
    cc: &mut CcTable,
    tsid: u16,
    own: Option<u16>,
    pid: u16,
    sec: &[u8],
    out: &mut Vec<u8>,
) {
    let reduced = match own {
        Some(sid) if pid == PAT_PID || pid == SDT_PID => cache.filter(tsid, sid, sec),
        _ => None,
    };
    cc.emit(pid, reduced.as_deref().unwrap_or(sec), out);
}

/// A PMT PID the stream watches (from the TS's PAT).
#[derive(Debug, Clone, Copy)]
struct PmtWatch {
    pid: u16,
    prog: u16,
    /// When the device last delivered it in this stream.
    dev: Option<Instant>,
}

/// The PMT PIDs of a PAT section; new ones count as not delivered in this stream.
fn pmts_from_pat(pmts: &mut Vec<PmtWatch>, sec: &[u8]) {
    for (prog, pid) in pat::pat_entries(sec).filter(|&(prog, _)| prog != 0) {
        if let Some(w) = pmts.iter_mut().find(|w| w.prog == prog) {
            w.pid = pid;
            continue;
        }
        if pmts.len() >= MAX_PMT {
            break;
        }
        pmts.push(PmtWatch {
            pid,
            prog,
            dev: None,
        });
    }
}

/// One PID's section reassembly.
#[derive(Debug)]
struct Asm {
    pid: u16,
    /// The section so far (empty: none in progress).
    buf: Vec<u8>,
    /// Its whole length once the 3-byte header is in.
    need: usize,
    /// The next expected continuity_counter.
    cc: Option<u8>,
}

impl Asm {
    fn new(pid: u16) -> Self {
        Self {
            pid,
            buf: Vec::with_capacity(SEC_MAX),
            need: 0,
            cc: None,
        }
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.need = 0;
    }

    /// Feed bytes; returns (bytes consumed, whether a section is complete). A section
    /// announcing more than [`SEC_MAX`] bytes is dropped with the rest of `b`.
    fn feed(&mut self, b: &[u8]) -> (usize, bool) {
        let mut used = 0;
        if self.buf.len() < 3 {
            used = (3 - self.buf.len()).min(b.len());
            self.buf.extend_from_slice(&b[..used]);
            if self.buf.len() < 3 {
                return (used, false);
            }
            self.need = pat::section_total(&self.buf);
            if self.need > SEC_MAX {
                self.reset();
                return (b.len(), false);
            }
        }
        let take = (self.need - self.buf.len()).min(b.len() - used);
        self.buf.extend_from_slice(&b[used..used + take]);
        (used + take, self.buf.len() == self.need)
    }
}

/// Re-sends of one stream.
#[derive(Debug, Default, Clone, Copy)]
struct Sent {
    pat: u64,
    pmt: u64,
    sdt: u64,
    nit: u64,
}

/// Per-call facts handed down to section handling.
#[derive(Clone, Copy)]
struct Ctx {
    tsid: u16,
    today: u32,
    now: Instant,
}

/// The cache and the per-stream re-send state.
pub struct PsiKeeper {
    cache: Cache,
    cc: CcTable,
    // ---- per stream (begin) ----
    /// The tuned satellite TS; `None` = keep nothing visible.
    tsid: Option<u16>,
    /// The TS is known silent (PSI fallback): start at once.
    force: bool,
    /// The tuned service whose PAT / SDT\[actual\] this stream re-sends reduced (the
    /// device's dropped). Left as delivered, they list every service of the TS, the host
    /// maps all of them onto the one channel line and the others show nothing (hardware
    /// 2026-10-20: the BS radio service 531 sat on the line of TV service 231, silent).
    own: Option<u16>,
    t0: Instant,
    /// When the device last delivered each table in this stream.
    dev_pat: Option<Instant>,
    dev_sdt: Option<Instant>,
    dev_nit: Option<Instant>,
    /// When we last re-sent each.
    inj_pat: Option<Instant>,
    inj_sdt: Option<Instant>,
    inj_nit: Option<Instant>,
    pmts: Vec<PmtWatch>,
    asm: Vec<Asm>,
    sent: Sent,
}

impl PsiKeeper {
    pub fn new() -> Self {
        Self {
            cache: Cache::default(),
            cc: CcTable::new(),
            tsid: None,
            force: false,
            own: None,
            t0: Instant::now(),
            dev_pat: None,
            dev_sdt: None,
            dev_nit: None,
            inj_pat: None,
            inj_sdt: None,
            inj_nit: None,
            pmts: Vec::new(),
            asm: Vec::new(),
            sent: Sent::default(),
        }
    }

    /// A stream starts on `tsid` (`None` = terrestrial: keep nothing visible).
    /// `known_silent`: the TS is known to deliver nothing (the PSI-only fallback after a
    /// silent single-service attempt) -- start re-sending at once.
    ///
    /// TSID 0 counts as `None`.
    pub fn begin(&mut self, tsid: Option<u16>, known_silent: bool, now: Instant) {
        let tsid = tsid.filter(|&t| t != 0);
        if tsid != self.tsid {
            self.cc.reset(); // another TS: its CCs start afresh
        }
        self.tsid = tsid;
        self.force = known_silent;
        self.own = None;
        self.t0 = now;
        self.dev_pat = None;
        self.dev_sdt = None;
        self.dev_nit = None;
        self.inj_pat = None;
        self.inj_sdt = None;
        self.inj_nit = None;
        self.asm.clear();
        self.pmts.clear();
        self.sent = Sent::default();
        if let Some(pat) = tsid.and_then(|t| self.cache.find(Key::new(Kind::Pat, t, 0))) {
            pmts_from_pat(&mut self.pmts, &pat.sec);
        }
    }

    /// Watch delivered, decrypted packets (cache their PSI). On an owned stream,
    /// blank the device's PID 0x0000 / 0x0011 packets in `buf` and append the reduced
    /// re-sends to `out`; the caller hands `out` to the host before `buf`.
    ///
    /// Continuity counters are noted per PID so that later re-sends continue the
    /// device's (a broken count is a drop to TVTest).
    pub fn observe(&mut self, buf: &mut [u8], view: &StreamView, now: Instant, out: &mut Vec<u8>) {
        let Some(tsid) = self.tsid else {
            return; // satellite only (terrestrial needs none)
        };
        self.own = view.program;
        let cx = Ctx {
            tsid,
            today: crate::today(),
            now,
        };
        let mut copy = [0u8; PACKET];
        for q in buf.as_chunks_mut::<PACKET>().0 {
            let (pid, afc, cc) = (packet::pid(q), packet::afc(q), packet::cc(q));
            if q[0] != SYNC || packet::tei(q) || afc & 1 == 0 {
                continue;
            }
            let p: &[u8] = if self.own.is_some() && (pid == PAT_PID || pid == SDT_PID) {
                // reassemble from a copy and blank the original: these PIDs now carry
                // only what `section` re-sends, with our own continuity counter
                copy.copy_from_slice(q);
                packet::make_null(q);
                &copy
            } else {
                self.cc.set(pid, cc);
                q
            };
            if pid == NULL_PID {
                continue;
            }
            if let Some(ai) = self.asm_for(pid) {
                self.packet(ai, p, cx, out);
            }
        }
    }

    /// The reassembler of `pid`, if it carries tables the keeper wants.
    fn asm_for(&mut self, pid: u16) -> Option<usize> {
        if let Some(i) = self.asm.iter().position(|a| a.pid == pid) {
            return Some(i);
        }
        let want =
            matches!(pid, PAT_PID | NIT_PID | SDT_PID) || self.pmts.iter().any(|w| w.pid == pid);
        if !want || self.asm.len() >= MAX_ASM {
            return None;
        }
        self.asm.push(Asm::new(pid));
        Some(self.asm.len() - 1)
    }

    /// One payload packet `q` of reassembler `ai`.
    fn packet(&mut self, ai: usize, q: &[u8], cx: Ctx, out: &mut Vec<u8>) {
        let cc = packet::cc(q);
        let a = &mut self.asm[ai];
        if a.cc.is_some_and(|want| want != cc) {
            a.reset(); // lost a packet
        }
        a.cc = Some((cc + 1) & 0x0F);
        let Some(mut s) = packet::payload_offset(q) else {
            return;
        };
        if packet::pusi(q) {
            // a section starts after the pointer_field; before it, the previous one ends
            let ptr = usize::from(q[s]);
            s += 1;
            if !self.asm[ai].buf.is_empty() && ptr > 0 && s + ptr <= PACKET {
                self.feed(ai, &q[s..s + ptr], cx, out);
            }
            self.asm[ai].reset();
            s += ptr;
            while s < PACKET && q[s] != 0xFF {
                // one or more sections; 0xFF is stuffing
                s += self.feed(ai, &q[s..], cx, out);
                if !self.asm[ai].buf.is_empty() {
                    break; // continues in the next packet
                }
            }
        } else if !self.asm[ai].buf.is_empty() {
            self.feed(ai, &q[s..], cx, out);
        }
    }

    /// Feed reassembler `ai`; handle a completed section. Returns bytes consumed.
    fn feed(&mut self, ai: usize, b: &[u8], cx: Ctx, out: &mut Vec<u8>) -> usize {
        let (used, done) = self.asm[ai].feed(b);
        if done {
            let pid = self.asm[ai].pid;
            let sec = std::mem::take(&mut self.asm[ai].buf);
            self.section(pid, &sec, cx, out);
            let a = &mut self.asm[ai];
            a.buf = sec;
            a.reset();
        }
        used
    }

    /// A complete section from the device on `pid`.
    fn section(&mut self, pid: u16, sec: &[u8], cx: Ctx, out: &mut Vec<u8>) {
        // long form, current (not next), CRC-valid
        if sec.len() < 12 || sec[1] & 0x80 == 0 || sec[5] & 0x01 == 0 || !crc::section_crc_ok(sec) {
            return;
        }
        let (tid, id) = (sec[0], be16(sec, 3));
        let (num, last) = (u16::from(sec[6]), u16::from(sec[7]));
        if pid == PAT_PID && tid == TID_PAT {
            self.dev_pat = Some(cx.now);
            if num == 0 && last == 0 {
                self.cache
                    .store(Key::new(Kind::Pat, id, 0), false, 0, sec, cx.today);
                pmts_from_pat(&mut self.pmts, sec);
            }
        } else if tid == TID_PMT {
            let mut watched = false;
            for w in self
                .pmts
                .iter_mut()
                .filter(|w| w.pid == pid && w.prog == id)
            {
                w.dev = Some(cx.now);
                watched = true;
            }
            if watched {
                self.cache
                    .store(Key::new(Kind::Pmt, cx.tsid, id), false, 0, sec, cx.today);
            }
        } else if pid == NIT_PID && tid == TID_NIT {
            self.dev_nit = Some(cx.now);
            self.cache
                .store(Key::new(Kind::Nit, id, num), false, last, sec, cx.today);
        } else if pid == SDT_PID && (tid == TID_SDT || tid == TID_SDT_OTHER) {
            if tid == TID_SDT && id == cx.tsid {
                self.dev_sdt = Some(cx.now);
            }
            self.cache.store(
                Key::new(Kind::Sdt, id, num),
                tid == TID_SDT_OTHER,
                last,
                sec,
                cx.today,
            );
        }
        // owned stream: the device's packets on these PIDs were blanked (observe);
        // re-send each of their sections, the PAT / SDT[actual] reduced
        if self.own.is_some() && (pid == PAT_PID || pid == SDT_PID) {
            send(&self.cache, &mut self.cc, cx.tsid, self.own, pid, sec, out);
        }
    }

    /// Called ~5x/s even without data: append what must be re-sent to `out`.
    ///
    /// Only a table the device has NOT delivered in this stream: the host keeps a table
    /// once it has it, and a re-sent copy on a PID the device also uses breaks that
    /// PID's continuity, which TVTest counts as a drop (hardware 2026-10-18: one SDT
    /// re-send during 3.5 min of a healthy CS stream whose SDT\[actual\] paused > 3 s).
    pub fn tick(&mut self, view: &StreamView, now: Instant, out: &mut Vec<u8>) {
        let Some(tsid) = self.tsid else {
            return;
        };
        self.own = view.program; // a silent stream never reaches observe
        if view.undecryptable {
            return;
        }
        let el = now.saturating_duration_since(self.t0);
        if !self.force && el < GRACE {
            return;
        }
        let silent = !view.delivered_any; // not one byte from the device so far
        let due = |last: Option<Instant>, every: Duration| {
            last.is_none_or(|t| now.saturating_duration_since(t) >= every)
        };
        let pat_missing = self.dev_pat.is_none();
        if pat_missing
            && (silent || el > PAT_AFTER)
            && due(
                self.inj_pat,
                if silent { PAT_EVERY_SILENT } else { PAT_EVERY },
            )
        {
            self.send_pat_pmt(tsid, out);
            self.inj_pat = Some(now);
        }
        if self.dev_sdt.is_none()
            && (silent || el > SDT_AFTER)
            && due(
                self.inj_sdt,
                if silent { SDT_EVERY_SILENT } else { SDT_EVERY },
            )
        {
            self.send_sdt(tsid, out);
            self.inj_sdt = Some(now);
        }
        if pat_missing && self.dev_nit.is_none() && el > NIT_AFTER && due(self.inj_nit, NIT_EVERY) {
            self.send_nit(tsid, out);
            self.inj_nit = Some(now);
        }
    }

    /// The PAT (cached, else built from the SDT service list), then the cached PMTs
    /// the device has not delivered in this stream.
    fn send_pat_pmt(&mut self, tsid: u16, out: &mut Vec<u8>) {
        if let Some(pat) = self.cache.find(Key::new(Kind::Pat, tsid, 0)) {
            pmts_from_pat(&mut self.pmts, &pat.sec);
            send(
                &self.cache,
                &mut self.cc,
                tsid,
                self.own,
                PAT_PID,
                &pat.sec,
                out,
            );
        } else {
            let set = self.cache.sdt_set(tsid, MAX_SDT_SET);
            if set.is_empty() {
                return; // nothing known about this TS
            }
            for (_, sid, _) in set.iter().flat_map(|e| sdt_services(&e.sec)) {
                if self.pmts.len() < MAX_PMT && !self.pmts.iter().any(|w| w.prog == sid) {
                    let pid = SYN_PMT + self.pmts.len() as u16;
                    self.pmts.push(PmtWatch {
                        pid,
                        prog: sid,
                        dev: None,
                    });
                }
            }
            let mut sec = long_header(TID_PAT, tsid, 0);
            sec.extend_from_slice(&[0x00, 0x00, 0xE0, 0x10]); // NIT
            for w in &self.pmts {
                if sec.len() + 8 > SEC_MAX {
                    break;
                }
                sec.extend_from_slice(&w.prog.to_be_bytes());
                sec.extend_from_slice(&(0xE000 | w.pid).to_be_bytes());
            }
            finish(&mut sec);
            send(
                &self.cache,
                &mut self.cc,
                tsid,
                self.own,
                PAT_PID,
                &sec,
                out,
            );
        }
        self.sent.pat += 1;
        // Only real (cached) PMTs, never a made-up one: TVTest's scan drops a service
        // whose PMT lists no playable video, while one whose PMT never arrives is
        // registered after the retries (hardware 2026-10-18: every service sent an
        // ES-less PMT was missing from the scan result).
        for w in self.pmts.iter().filter(|w| w.dev.is_none()) {
            if let Some(pmt) = self.cache.find(Key::new(Kind::Pmt, tsid, w.prog)) {
                self.cc.emit(w.pid, &pmt.sec, out);
                self.sent.pmt += 1;
            }
        }
    }

    /// The SDT sections of the tuned TS; an SDT\[other\] copy re-labelled \[actual\].
    fn send_sdt(&mut self, tsid: u16, out: &mut Vec<u8>) {
        for e in self.cache.sdt_set(tsid, MAX_SDT_SET) {
            if e.other {
                let mut tmp = e.sec.clone();
                tmp[0] = TID_SDT;
                crc::set_section_crc(&mut tmp);
                send(
                    &self.cache,
                    &mut self.cc,
                    tsid,
                    self.own,
                    SDT_PID,
                    &tmp,
                    out,
                );
            } else {
                send(
                    &self.cache,
                    &mut self.cc,
                    tsid,
                    self.own,
                    SDT_PID,
                    &e.sec,
                    out,
                );
            }
            self.sent.sdt += 1;
        }
    }

    /// The NIT sections of the network the TS's SDT names (original_network_id).
    fn send_nit(&mut self, tsid: u16, out: &mut Vec<u8>) {
        let Some(&sdt) = self.cache.sdt_set(tsid, 1).first() else {
            return;
        };
        let onid = be16(&sdt.sec, 8);
        let mut nit: Vec<&Entry> = self
            .cache
            .ent
            .iter()
            .filter(|e| e.is(Kind::Nit, onid) && e.key.sub < 256)
            .collect();
        nit.sort_by_key(|e| e.key.sub);
        for e in nit {
            self.cc.emit(NIT_PID, &e.sec, out);
            self.sent.nit += 1;
        }
    }

    /// One log line at the end of a satellite stream (what was re-sent, cache size).
    pub fn end_summary(&self) -> Option<String> {
        let tsid = self.tsid?;
        let (mut sdt_ts, mut sdt_other, mut pats, mut pmts, mut nits) = (0, 0, 0, 0, 0);
        for e in &self.cache.ent {
            match e.key.kind {
                Kind::Pat => pats += 1,
                Kind::Pmt => pmts += 1,
                Kind::Nit => nits += 1,
                Kind::Sdt if e.key.sub == 0 => {
                    sdt_ts += 1;
                    sdt_other += usize::from(e.other);
                }
                Kind::Sdt => {}
            }
        }
        let s = self.sent;
        Some(format!(
            "psi: TS {tsid:04x} -- re-sent PAT x{}, PMT x{}, SDT x{}, NIT x{} | cache: SDT of {sdt_ts} TS(s) \
             ({sdt_other} only via SDT[other]), {pats} PAT(s), {pmts} PMT(s), {nits} NIT section(s)",
            s.pat, s.pmt, s.sdt, s.nit
        ))
    }

    /// All cached sections; clears [`PsiKeeper::changed`].
    pub fn export(&mut self) -> Vec<PsiRecord> {
        self.cache.dirty = false;
        self.cache
            .ent
            .iter()
            .map(|e| PsiRecord {
                kind: e.key.kind as u8,
                other: e.other,
                id: e.key.id,
                sub: e.key.sub,
                seen_day: e.seen,
                section: e.sec.clone(),
            })
            .collect()
    }

    /// Take one cached section back (`today` = Unix day). False: bad CRC / kind, or
    /// not seen for [`crate::PSI_KEEP_DAYS`].
    ///
    /// Sections not delivered for that long are dropped: a TS that moved or closed
    /// would otherwise be re-sent forever. An import does not count as a change.
    pub fn import(&mut self, rec: &PsiRecord, today: u32) -> bool {
        let Some(kind) = Kind::from_u8(rec.kind) else {
            return false;
        };
        if rec.section.len() > SEC_MAX || !crc::section_crc_ok(&rec.section) {
            return false;
        }
        if u64::from(rec.seen_day) + u64::from(PSI_KEEP_DAYS) < u64::from(today) {
            return false; // gone from the air
        }
        // the section's own last_section_number (255 here would let a malformed
        // section through)
        let last = match kind {
            Kind::Nit | Kind::Sdt => u16::from(rec.section[7]),
            _ => 255,
        };
        self.cache.store(
            Key::new(kind, rec.id, rec.sub),
            rec.other,
            last,
            &rec.section,
            rec.seen_day,
        );
        self.cache.dirty = false; // loaded, not changed
        true
    }

    /// Sections were added or refreshed since the last export / import.
    pub fn changed(&self) -> bool {
        self.cache.dirty
    }
}

impl Default for PsiKeeper {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PsiKeeper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PsiKeeper")
            .field("sections", &self.cache.ent.len())
            .field("changed", &self.cache.dirty)
            .field("tsid", &self.tsid)
            .field("own", &self.own)
            .field("sent", &self.sent)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
