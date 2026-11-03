//! Terrestrial logos: each channel's CDT (PID 0x29) carries its station's logos, one
//! section per logo type, and the SDT's logo_transmission_descriptor names the logo
//! of each service. The sections of one round come about 100 s apart, so a round of
//! six types takes about 10 minutes.

use std::collections::{BTreeMap, BTreeSet};

use super::carousel::{Logo, be16, descriptors};
use crate::lineup::sdt_section;

pub(super) const PID_SDT: u16 = 0x0011;
pub(super) const PID_CDT: u16 = 0x0029;
pub(super) const TID_SDT: u8 = 0x42;
pub(super) const TID_CDT: u8 = 0xC8;
pub(super) const DESC_LOGO_TRANSMISSION: u8 = 0xCF;
/// CDT data_type of logo data.
pub(super) const DATA_TYPE_LOGO: u8 = 0x01;
/// logo_type 0..=5.
const LOGO_TYPES: usize = 6;

/// (original_network_id, transport_stream_id, service_id)
type Service = (u16, u16, u16);

#[derive(Debug, Default)]
pub(super) struct Cdt {
    /// (original_network_id, logo_id) -> the services showing it
    services: BTreeMap<(u16, u16), BTreeSet<Service>>,
    /// (original_network_id, logo_id, logo_type) -> PNG
    logos: BTreeMap<(u16, u16, u8), Vec<u8>>,
    /// The logo that came first.
    first: Option<(u16, u16, u8)>,
    /// The first logo came again: the CDT has gone round once.
    cycled: bool,
}

impl Cdt {
    /// SDT[actual]: the logo of each service with a logo_transmission_descriptor of
    /// transmission type 1 or 2 (CDT; type 3 is a logo string).
    pub(super) fn sdt(&mut self, sec: &[u8]) -> Option<()> {
        let (sdt, loops) = sdt_section(sec)?;
        if !sdt.actual {
            return None;
        }
        for (sid, desc) in loops {
            for (tag, d) in descriptors(desc) {
                if tag == DESC_LOGO_TRANSMISSION && matches!(d.first(), Some(1 | 2)) {
                    let logo_id = be16(d, 1)? & 0x01FF;
                    self.services
                        .entry((sdt.onid, logo_id))
                        .or_default()
                        .insert((sdt.onid, sdt.tsid, sid));
                }
            }
        }
        Some(())
    }

    /// A CDT section of logo data: logo_type, logo_id, logo_version, then the PNG.
    pub(super) fn cdt(&mut self, sec: &[u8]) -> Option<()> {
        // a long section, current (not the next version)
        if sec.get(1)? & 0x80 == 0 || sec.get(5)? & 0x01 == 0 {
            return None;
        }
        let onid = be16(sec, 8)?;
        if *sec.get(10)? != DATA_TYPE_LOGO {
            return None;
        }
        let start = 13 + usize::from(be16(sec, 11)? & 0x0FFF);
        let m = sec.get(start..sec.len().checked_sub(4)?)?;
        let logo_type = *m.first()?;
        let logo_id = be16(m, 1)? & 0x01FF;
        let png = m.get(7..7 + usize::from(be16(m, 5)?))?;
        if logo_type > 5 || png.is_empty() {
            return None;
        }
        let key = (onid, logo_id, logo_type);
        self.logos.insert(key, png.to_vec());
        match self.first {
            None => self.first = Some(key),
            Some(first) if first == key => self.cycled = true,
            Some(_) => {}
        }
        Some(())
    }

    /// Every logo the CDT carries is in: each logo the SDT names has all six types, or
    /// the CDT has gone round once (the first logo came again; a station may send
    /// fewer types or some logos not at all). Not on any repeat: a station may send one
    /// type more often than the others.
    pub(super) fn done(&self) -> bool {
        let all_types = |&(onid, id): &(u16, u16)| {
            self.logos
                .range((onid, id, 0)..=(onid, id, u8::MAX))
                .count()
                == LOGO_TYPES
        };
        self.cycled || (!self.services.is_empty() && self.services.keys().all(all_types))
    }

    pub(super) fn has_logos(&self) -> bool {
        !self.logos.is_empty()
    }

    /// e.g. "CDT logos: 32736/0 types [0, 1, 2]".
    pub(super) fn progress(&self) -> String {
        let mut types: BTreeMap<(u16, u16), Vec<u8>> = BTreeMap::new();
        for &(onid, id, t) in self.logos.keys() {
            types.entry((onid, id)).or_default().push(t);
        }
        let parts: Vec<_> = types
            .iter()
            .map(|((onid, id), t)| format!("{onid}/{id} types {t:?}"))
            .collect();
        let round = if self.done() { " (all in)" } else { "" };
        format!("CDT logos: {}{round}", parts.join(", "))
    }

    /// The logos received for the services that show them.
    pub(super) fn logos(&self) -> Vec<Logo> {
        self.logos
            .iter()
            .filter_map(|(&(onid, logo_id, logo_type), png)| {
                let services = self.services.get(&(onid, logo_id))?;
                Some(Logo {
                    network_id: onid,
                    logo_id,
                    logo_type,
                    services: services.iter().copied().collect(),
                    png: png.clone(),
                })
            })
            .collect()
    }
}
