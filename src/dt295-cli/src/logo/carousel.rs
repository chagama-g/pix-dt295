//! Receiving the logos: the logo carousels of the engineering service, assembled from
//! its TS (PAT -> PMT -> DSM-CC DII / DDB -> logo modules), and the CDT of a terrestrial
//! channel (`cdt.rs`).

use std::collections::{BTreeMap, HashMap};

use dt295::ts::crc::section_crc_ok;
use dt295::ts::packet;
use dt295::ts::pat::parse_pat;

use crate::sections::Packets;
pub(super) use crate::sections::SectionBuf;
pub(super) use dt295::ts::section::descriptors;

use super::cdt::{Cdt, PID_CDT, PID_SDT, TID_CDT, TID_SDT};

/// component_tag of the carousels with the BS / CS logos.
pub(super) const TAG_BS: u8 = 0x79;
pub(super) const TAG_CS: u8 = 0x7A;
pub(super) const STREAM_TYPE_CAROUSEL: u8 = 0x0D;
pub(super) const DESC_STREAM_ID: u8 = 0x52;
pub(super) const DESC_MODULE_NAME: u8 = 0x02;
pub(super) const PID_PAT: u16 = 0x0000;
pub(super) const PID_TOT: u16 = 0x0014;
pub(super) const TID_PMT: u8 = 0x02;
pub(super) const TID_DII: u8 = 0x3B;
pub(super) const TID_DDB: u8 = 0x3C;
pub(super) const TID_TDT: u8 = 0x70;
pub(super) const TID_TOT: u8 = 0x73;
pub(super) const MSG_DII: u16 = 0x1002;
pub(super) const MSG_DDB: u16 = 0x1003;
/// Days from 1601-01-01 (TVTest's time base) to MJD 0 (1858-11-17).
pub(super) const DAYS_1601_TO_MJD0: u64 = 94_187;

pub(super) fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

pub(super) fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// A TDT / TOT JST_time (MJD and BCD h:m:s) as seconds since 1601-01-01.
pub(super) fn linear_seconds(t: &[u8]) -> Option<u64> {
    let mjd = u64::from(be16(t, 0)?);
    let bcd = |b: u8| (b >> 4 < 10 && b & 15 < 10).then(|| u64::from((b >> 4) * 10 + (b & 15)));
    let (h, m, s) = (bcd(*t.get(2)?)?, bcd(*t.get(3)?)?, bcd(*t.get(4)?)?);
    (h < 24 && m < 60 && s < 61).then(|| (DAYS_1601_TO_MJD0 + mjd) * 86_400 + h * 3600 + m * 60 + s)
}

/// One logo as broadcast: a PNG for some services.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Logo {
    pub(super) network_id: u16,
    pub(super) logo_id: u16,
    pub(super) logo_type: u8,
    /// (original_network_id, transport_stream_id, service_id)
    pub(super) services: Vec<(u16, u16, u16)>,
    pub(super) png: Vec<u8>,
}

/// Whether a module name is one of the logo modules (LOGO-0x / CS_LOGO-0x).
pub(super) fn is_logo_module(name: &[u8]) -> bool {
    (name.len() == 7 && name.starts_with(b"LOGO-0"))
        || (name.len() == 10 && name.starts_with(b"CS_LOGO-0"))
}

/// The logos of a complete logo module (logo_type, then the logo loop). Entries
/// without a service or without data are skipped, as TVTest does.
pub(super) fn parse_logo_module(body: &[u8]) -> Vec<Logo> {
    let mut out = Vec::new();
    let (Some(&logo_type), Some(count)) = (body.first(), be16(body, 1)) else {
        return out;
    };
    if logo_type > 5 {
        return out;
    }
    let mut pos = 3;
    for _ in 0..count {
        let (Some(id), Some(&n)) = (be16(body, pos), body.get(pos + 2)) else {
            break;
        };
        pos += 3;
        let n = usize::from(n);
        let Some(svc) = body.get(pos..pos + 6 * n) else {
            break;
        };
        let services: Vec<_> = svc
            .as_chunks::<6>()
            .0
            .iter()
            .map(|s| {
                let w = |i: usize| u16::from_be_bytes([s[i], s[i + 1]]);
                (w(0), w(2), w(4))
            })
            .collect();
        pos += 6 * n;
        let Some(size) = be16(body, pos) else {
            break;
        };
        pos += 2;
        let Some(png) = body.get(pos..pos + usize::from(size)) else {
            break;
        };
        pos += usize::from(size);
        if let (Some(&(network_id, ..)), false) = (services.first(), png.is_empty()) {
            out.push(Logo {
                network_id,
                logo_id: id & 0x01FF,
                logo_type,
                services,
                png: png.to_vec(),
            });
        }
    }
    out
}

/// A module of a carousel: its DII entry (once seen) and the blocks received.
#[derive(Debug, Default)]
pub(super) struct Module {
    /// From the DII; `None` until then (DDBs can come first).
    name: Option<Vec<u8>>,
    size: u32,
    version: u8,
    block_size: u16,
    /// block number -> (module version, data)
    blocks: BTreeMap<u16, (u8, Vec<u8>)>,
}

impl Module {
    fn is_logo(&self) -> bool {
        self.name.as_deref().is_some_and(is_logo_module)
    }

    fn block_count(&self) -> usize {
        let bs = usize::from(self.block_size);
        if bs == 0 {
            return 0;
        }
        (self.size as usize).div_ceil(bs)
    }

    /// The module's bytes, once every block of the current version is here.
    fn body(&self) -> Option<Vec<u8>> {
        let n = self.block_count();
        if self.name.is_none() || n == 0 {
            return None;
        }
        let mut body = Vec::with_capacity(self.size as usize);
        for i in 0..n {
            let (v, data) = self.blocks.get(&u16::try_from(i).ok()?)?;
            if *v != self.version {
                return None;
            }
            body.extend_from_slice(data);
        }
        (body.len() >= self.size as usize).then(|| {
            body.truncate(self.size as usize);
            body
        })
    }
}

/// One logo carousel (a PMT ES with component_tag 0x79 or 0x7A).
#[derive(Debug)]
pub(super) struct Carousel {
    tag: u8,
    /// (download_id, module_id) -> module
    modules: BTreeMap<(u32, u16), Module>,
}

impl Carousel {
    /// (complete, total) logo modules; total 0 before the DII.
    fn count(&self) -> (usize, usize) {
        let logo = self.modules.values().filter(|m| m.is_logo());
        let total = logo.clone().count();
        (logo.filter(|m| m.body().is_some()).count(), total)
    }
}

/// Collects the logo modules from the TS of the engineering service.
#[derive(Debug, Default)]
pub(super) struct Collector {
    program: u16,
    packets: Packets,
    sections: HashMap<u16, SectionBuf>,
    pmt_pid: Option<u16>,
    /// ES PID -> carousel
    carousels: BTreeMap<u16, Carousel>,
    /// The CDT logos and the SDT's logo of each service (terrestrial).
    cdt: Cdt,
    /// The broadcast time (TDT / TOT), as TVTest's time.
    pub(super) time: Option<u64>,
    /// Valid PAT sections seen. They come every 0.1 s, so this standing still means
    /// the stream stalled (or does not decrypt).
    pub(super) pats: u64,
}

impl Collector {
    pub(super) fn new(program: u16) -> Self {
        Self {
            program,
            ..Self::default()
        }
    }

    /// Process TS bytes (any length; a packet cut at the end is kept for the next call).
    pub(super) fn feed(&mut self, data: &[u8]) {
        let mut packets = std::mem::take(&mut self.packets);
        packets.feed(data, |p| self.packet(p));
        self.packets = packets;
    }

    fn packet(&mut self, p: &[u8]) {
        if packet::tei(p) {
            return;
        }
        let pid = packet::pid(p);
        let wanted = pid == PID_PAT
            || pid == PID_TOT
            || pid == PID_SDT
            || pid == PID_CDT
            || Some(pid) == self.pmt_pid
            || self.carousels.contains_key(&pid);
        if !wanted {
            return;
        }
        let sections = self.sections.entry(pid).or_default().push(p);
        for sec in sections {
            self.section(pid, &sec);
        }
    }

    fn section(&mut self, pid: u16, sec: &[u8]) {
        let Some(&tid) = sec.first() else {
            return;
        };
        let long = sec.get(1).is_some_and(|b| b & 0x80 != 0);
        if long && !section_crc_ok(sec) {
            return;
        }
        match (pid, tid) {
            (PID_PAT, 0x00) => {
                self.pats += 1;
                self.pmt_pid = parse_pat(sec)
                    .into_iter()
                    .find(|&(prog, _)| prog == self.program)
                    .map(|(_, pmt)| pmt);
            }
            (PID_TOT, TID_TDT | TID_TOT) => {
                if let Some(t) = sec.get(3..8).and_then(linear_seconds) {
                    self.time = Some(t);
                }
            }
            (PID_SDT, TID_SDT) => {
                let _ = self.cdt.sdt(sec);
            }
            (PID_CDT, TID_CDT) => {
                let _ = self.cdt.cdt(sec);
            }
            (_, TID_PMT) if Some(pid) == self.pmt_pid => self.pmt(sec),
            (_, TID_DII) => {
                let _ = self.dii(pid, sec);
            }
            (_, TID_DDB) => {
                let _ = self.ddb(pid, sec);
            }
            _ => {}
        }
    }

    /// Register the PMT's logo carousels (stream_type 0x0D, component_tag 0x79 / 0x7A).
    fn pmt(&mut self, sec: &[u8]) {
        if be16(sec, 3) != Some(self.program) {
            return;
        }
        let end = sec.len().saturating_sub(4);
        let Some(info) = be16(sec, 10) else {
            return;
        };
        let mut j = 12 + usize::from(info & 0x0FFF);
        while j + 5 <= end {
            let st = sec[j];
            let es_pid = u16::from_be_bytes([sec[j + 1], sec[j + 2]]) & 0x1FFF;
            let len = usize::from(u16::from_be_bytes([sec[j + 3], sec[j + 4]]) & 0x0FFF);
            let desc = sec.get(j + 5..(j + 5 + len).min(end)).unwrap_or_default();
            j += 5 + len;
            let tag = descriptors(desc)
                .find(|(t, _)| *t == DESC_STREAM_ID)
                .and_then(|(_, d)| d.first().copied());
            if let (STREAM_TYPE_CAROUSEL, Some(tag @ (TAG_BS | TAG_CS))) = (st, tag) {
                self.carousels.entry(es_pid).or_insert_with(|| Carousel {
                    tag,
                    modules: BTreeMap::new(),
                });
            }
        }
    }

    /// DownloadInfoIndication: the modules of a carousel, with their names.
    fn dii(&mut self, pid: u16, sec: &[u8]) -> Option<()> {
        let car = self.carousels.get_mut(&pid)?;
        let m = sec.get(8..sec.len().checked_sub(4)?)?;
        if be16(m, 2)? != MSG_DII {
            return None;
        }
        let mut j = 12 + usize::from(*m.get(9)?);
        let download_id = be32(m, j)?;
        let block_size = be16(m, j + 4)?;
        j += 16;
        j += 2 + usize::from(be16(m, j)?);
        let count = be16(m, j)?;
        j += 2;
        // (module id, size, version, name)
        let mut listed = Vec::new();
        for _ in 0..count {
            let id = be16(m, j)?;
            let size = be32(m, j + 2)?;
            let version = *m.get(j + 6)?;
            let info_len = usize::from(*m.get(j + 7)?);
            let info = m.get(j + 8..j + 8 + info_len)?;
            j += 8 + info_len;
            if let Some((_, name)) = descriptors(info).find(|(t, _)| *t == DESC_MODULE_NAME) {
                listed.push((id, size, version, name.to_vec()));
            }
        }
        // A new download of the logos replaces the old one, whose partial modules would
        // never complete. Between transmissions the carousel sends DIIs without any
        // module (download_id 0): those change nothing.
        if listed.iter().any(|(.., name)| is_logo_module(name)) {
            car.modules.retain(|&(d, _), _| d == download_id);
        }
        for (id, size, version, name) in listed {
            let module = car.modules.entry((download_id, id)).or_default();
            let same = module.name.as_deref() == Some(&name[..])
                && (module.size, module.version, module.block_size) == (size, version, block_size);
            if !same {
                // A changed module starts over; blocks that came before its first DII
                // are kept if they are of this version.
                let mut blocks = std::mem::take(&mut module.blocks);
                if module.name.is_some() {
                    blocks.clear();
                }
                blocks.retain(|_, (v, _)| *v == version);
                *module = Module {
                    name: Some(name),
                    size,
                    version,
                    block_size,
                    blocks,
                };
            }
        }
        Some(())
    }

    /// DownloadDataBlock: one block of a module.
    fn ddb(&mut self, pid: u16, sec: &[u8]) -> Option<()> {
        let car = self.carousels.get_mut(&pid)?;
        let m = sec.get(8..sec.len().checked_sub(4)?)?;
        if be16(m, 2)? != MSG_DDB {
            return None;
        }
        let download_id = be32(m, 4)?;
        let j = 12 + usize::from(*m.get(9)?);
        let id = be16(m, j)?;
        let version = *m.get(j + 2)?;
        let block = be16(m, j + 4)?;
        let data = m.get(j + 6..)?;
        let module = car.modules.entry((download_id, id)).or_default();
        if module.name.is_some() && module.version != version {
            return None; // an old block, or ahead of its DII
        }
        module.blocks.insert(block, (version, data.to_vec()));
        Some(())
    }

    /// The engineering service's TS: every logo carousel listed in the PMT has its DII
    /// and all its modules. Any other TS (a terrestrial channel): its CDT is in.
    pub(super) fn done(&self) -> bool {
        if self.pmt_pid.is_none() {
            return self.pats > 0 && self.cdt.done();
        }
        !self.carousels.is_empty()
            && self.carousels.values().all(|c| {
                let (complete, total) = c.count();
                total > 0 && complete == total
            })
    }

    /// e.g. "logo data: BS 6/6, CS 2/6 modules"; for a TS without the engineering
    /// service, its CDT's.
    pub(super) fn progress(&self) -> String {
        if self.pmt_pid.is_none() {
            if self.pats == 0 {
                return format!("waiting for service {}", self.program);
            }
            if !self.cdt.has_logos() {
                return format!("no service {} in this TS, no CDT logo yet", self.program);
            }
            return self.cdt.progress();
        }
        if self.carousels.is_empty() {
            return "waiting for the PMT's logo carousels".into();
        }
        let parts: Vec<_> = self
            .carousels
            .values()
            .map(|c| {
                let label = if c.tag == TAG_BS { "BS" } else { "CS" };
                match c.count() {
                    (_, 0) => format!("{label} -"),
                    (done, total) => format!("{label} {done}/{total}"),
                }
            })
            .collect();
        format!("logo data: {} modules", parts.join(", "))
    }

    /// The logos of every complete logo module.
    pub(super) fn logos(&self) -> Vec<Logo> {
        self.carousels
            .values()
            .flat_map(|c| c.modules.values())
            .filter(|m| m.is_logo())
            .filter_map(Module::body)
            .flat_map(|b| parse_logo_module(&b))
            .collect()
    }

    /// The logos of the CDT, for the services the SDT gives them to.
    pub(super) fn cdt_logos(&self) -> Vec<Logo> {
        self.cdt.logos()
    }
}
