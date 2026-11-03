//! What a channel scan reads from the TS of one tune: the PAT and the SDT sections.

#![cfg_attr(not(unix), allow(dead_code))]

use std::collections::{BTreeMap, HashMap};

use dt295::ts::crc::section_crc_ok;
use dt295::ts::packet;
use dt295::ts::pat::parse_pat;

use crate::lineup::{Sdt, parse_sdt};
use crate::sections::{Packets, SectionBuf};

const PID_PAT: u16 = 0x0000;
const PID_SDT: u16 = 0x0011;
const TID_PAT: u8 = 0x00;

#[derive(Debug, Default)]
pub(super) struct Psi {
    packets: Packets,
    sections: HashMap<u16, SectionBuf>,
    /// The PAT's programs and their PMT PIDs, in PAT order (program 0, the NIT, left
    /// out).
    pub(super) pat: Option<Vec<(u16, u16)>>,
    /// The SDT sections of each sub-table, by (actual, transport_stream_id) and
    /// section_number; a new version replaces the sub-table.
    sdts: BTreeMap<(bool, u16), BTreeMap<u8, Sdt>>,
}

/// One whole SDT sub-table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Table {
    pub(super) actual: bool,
    pub(super) tsid: u16,
    pub(super) onid: u16,
    /// (service_id, service_type), in section order.
    pub(super) services: Vec<(u16, Option<u8>)>,
}

impl Psi {
    /// Process TS bytes (any length).
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
        if pid != PID_PAT && pid != PID_SDT {
            return;
        }
        for sec in self.sections.entry(pid).or_default().push(p) {
            if !section_crc_ok(&sec) {
                continue;
            }
            match (pid, sec[0]) {
                (PID_PAT, TID_PAT) => {
                    let programs: Vec<_> = parse_pat(&sec)
                        .into_iter()
                        .filter(|&(prog, _)| prog != 0)
                        .collect();
                    self.pat = Some(programs);
                }
                (PID_SDT, _) => {
                    if let Some(s) = parse_sdt(&sec) {
                        let table = self.sdts.entry((s.actual, s.tsid)).or_default();
                        if table.values().any(|old| old.version != s.version) {
                            table.clear();
                        }
                        table.insert(s.section_number, s);
                    }
                }
                _ => {}
            }
        }
    }

    /// The SDT[actual] came, whole.
    pub(super) fn sdt_actual_complete(&self) -> bool {
        let mut actual = self.sdts.iter().filter(|((a, _), _)| *a).peekable();
        actual.peek().is_some() && actual.all(|(_, t)| whole(t))
    }

    /// The whole SDT sub-tables.
    pub(super) fn tables(&self) -> Vec<Table> {
        self.sdts
            .iter()
            .filter(|(_, t)| whole(t))
            .filter_map(|(&(actual, tsid), t)| {
                Some(Table {
                    actual,
                    tsid,
                    onid: t.values().next()?.onid,
                    services: t
                        .values()
                        .flat_map(|s| s.services.iter().copied())
                        .collect(),
                })
            })
            .collect()
    }
}

/// A sub-table has every section from 0 to last_section_number.
fn whole(t: &BTreeMap<u8, Sdt>) -> bool {
    t.values().next().is_some_and(|s| {
        let last = s.last_section_number;
        t.len() == usize::from(last) + 1 && t.keys().all(|&n| n <= last)
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use dt295::ts::crc::set_section_crc;
    use dt295::ts::packet::PACKET;

    fn long_section(table_id: u8, body: &[u8]) -> Vec<u8> {
        let mut sec = vec![table_id, 0, 0];
        sec.extend_from_slice(body);
        sec.extend_from_slice(&[0; 4]);
        let sl = sec.len() - 3;
        sec[1] = 0xB0 | (sl >> 8) as u8;
        sec[2] = sl as u8;
        set_section_crc(&mut sec);
        sec
    }

    /// A TS built section by section (each from the start of its packets), one
    /// continuity counter per PID.
    #[derive(Default)]
    pub(in crate::mirakc) struct Ts {
        pub(in crate::mirakc) bytes: Vec<u8>,
        cc: HashMap<u16, u8>,
    }

    impl Ts {
        pub(in crate::mirakc) fn add(&mut self, pid: u16, sec: &[u8]) -> &mut Self {
            let mut payload = vec![0u8];
            payload.extend_from_slice(sec);
            for (i, chunk) in payload.chunks(184).enumerate() {
                let cc = self.cc.entry(pid).or_insert(0);
                let start = if i == 0 { 0x40 } else { 0 };
                self.bytes.extend_from_slice(&[
                    0x47,
                    start | (pid >> 8) as u8,
                    pid as u8,
                    0x10 | *cc,
                ]);
                *cc = (*cc + 1) & 15;
                self.bytes.extend_from_slice(chunk);
                self.bytes
                    .resize(self.bytes.len().next_multiple_of(PACKET), 0xFF);
            }
            self
        }
    }

    pub(in crate::mirakc) fn pat(tsid: u16, programs: &[(u16, u16)]) -> Vec<u8> {
        let mut body = tsid.to_be_bytes().to_vec();
        body.extend_from_slice(&[0xC1, 0, 0]);
        for &(prog, pid) in programs {
            body.extend_from_slice(&prog.to_be_bytes());
            body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
        }
        long_section(TID_PAT, &body)
    }

    /// An SDT section, version 0: (service_id, service_type) entries.
    pub(in crate::mirakc) fn sdt(
        actual: bool,
        tsid: u16,
        onid: u16,
        sections: (u8, u8),
        services: &[(u16, u8)],
    ) -> Vec<u8> {
        sdt_version(0, actual, tsid, onid, sections, services)
    }

    fn sdt_version(
        version: u8,
        actual: bool,
        tsid: u16,
        onid: u16,
        (number, last): (u8, u8),
        services: &[(u16, u8)],
    ) -> Vec<u8> {
        let mut body = tsid.to_be_bytes().to_vec();
        body.extend_from_slice(&[0xC1 | version << 1, number, last]);
        body.extend_from_slice(&onid.to_be_bytes());
        body.push(0xFF);
        for &(sid, t) in services {
            let desc = [0x48, 3, t, 0, 0];
            body.extend_from_slice(&sid.to_be_bytes());
            body.push(0xE3);
            body.extend_from_slice(&(0x8000 | desc.len() as u16).to_be_bytes());
            body.extend_from_slice(&desc);
        }
        long_section(if actual { 0x42 } else { 0x46 }, &body)
    }

    #[test]
    fn pat_and_sdt_sections() {
        let mut ts = Ts::default();
        ts.add(0, &pat(0x7FE0, &[(0, 0x10), (1024, 0x1F0), (1025, 0x1F1)]))
            .add(0x11, &sdt(true, 0x7FE0, 32736, (0, 1), &[(1024, 1)]))
            .add(0x11, &sdt(false, 0x7FE1, 32736, (0, 0), &[(1032, 1)]));

        let mut psi = Psi::default();
        // cut anywhere
        for chunk in ts.bytes.chunks(100) {
            psi.feed(chunk);
        }
        assert_eq!(
            psi.pat.as_deref(),
            Some(&[(1024, 0x1F0), (1025, 0x1F1)][..])
        );
        assert!(
            !psi.sdt_actual_complete(),
            "[actual] section 1 of 0..=1 is missing, though [other] is whole"
        );
        assert_eq!(psi.tables().len(), 1);
        let fed = ts.bytes.len();
        ts.add(0x11, &sdt(true, 0x7FE0, 32736, (1, 1), &[(1025, 1)]));
        psi.feed(&ts.bytes[fed..]);
        assert!(psi.sdt_actual_complete());
        let actual = psi.tables().into_iter().find(|t| t.actual).unwrap();
        assert_eq!(
            (actual.tsid, actual.onid, actual.services),
            (0x7FE0, 32736, vec![(1024, Some(1)), (1025, Some(1))])
        );
    }

    #[test]
    fn a_new_sdt_version_replaces_the_sub_table() {
        let mut ts = Ts::default();
        ts.add(0x11, &sdt_version(1, true, 1, 4, (0, 2), &[(101, 1)]))
            .add(0x11, &sdt_version(1, true, 1, 4, (2, 2), &[(103, 1)]))
            // version 2 has two sections only
            .add(0x11, &sdt_version(2, true, 1, 4, (0, 1), &[(101, 1)]))
            .add(0x11, &sdt_version(2, true, 1, 4, (1, 1), &[(102, 1)]));
        let mut psi = Psi::default();
        psi.feed(&ts.bytes);
        assert!(psi.sdt_actual_complete());
        assert_eq!(psi.tables()[0].services, [(101, Some(1)), (102, Some(1))]);
    }

    #[test]
    fn nothing_yet() {
        let psi = Psi::default();
        assert!(!psi.sdt_actual_complete());
        assert!(psi.pat.is_none());
        assert!(psi.tables().is_empty());
    }
}
