//! Channels shared by the BonDriver and the `dt295` command: the built-in ISDB-T
//! tables (UHF 13-62ch, CATV C13-C63), the BS / CS110 frequency plan and the
//! transponder of a satellite TS ([`transponder`]), the value of a satellite ini line
//! (`IF_kHz,TSID[,ServiceID]`, read with the number syntax of C `wcstoul(.., 0)`), and
//! the tuning spaces of a `BonDriver_DT295.ini` ([`ini_spaces`]): its own ([`spaces`]),
//! or those of the channel file of another BonDriver it names ([`file_spaces`]).

use std::fmt::Display;
use std::path::Path;

pub use dt295_chdef::num::{strtol, strtoul};
pub use dt295_chdef::plan::{
    CATV, LNB_LO_KHZ, SAT_IF_KHZ, UHF, bs_if_khz, catv_hz, nd_if_khz, uhf_hz,
};
use dt295_chdef::{ChannelFile, Ts, Tuning};

use crate::System;
use crate::profile::{self, Ini};

/// TSID of a tune that selects none (ISDB-T, or a satellite line without one).
pub const NO_TSID: u16 = 0xFFFF;
/// A satellite TSID of this or less is a TS number: the TS whose TSID has these low 3
/// bits (as BonDriver_PTx-ST_mod reads a TSID of 7 or less).
pub const MAX_TS_NUMBER: u16 = dt295_chdef::MAX_TS_NUMBER as u16;
/// original_network_id of BS and of the two CS110 networks.
const ONID_BS: u16 = 4;
const ONID_CS: [u16; 2] = [6, 7];

/// A right-hand BS transponder or CS110 ND: BS-`num` or ND`num`, and its IF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Transponder {
    pub bs: bool,
    pub num: u16,
    pub if_khz: u32,
}

impl Transponder {
    fn line_name(&self, rest: impl Display) -> String {
        let kind = if self.bs { "BS" } else { "ND" };
        format!("{kind}{:02}/{rest}", self.num)
    }

    /// The name of a one-TS ini line (`BS15/TS1`, `ND02/TS0`).
    pub fn ts_name(&self, tsid: u16) -> String {
        self.line_name(format_args!("TS{}", rel_ts(tsid)))
    }

    /// The name of a one-service ini line (`BS15/101`).
    pub fn service_name(&self, sid: u16) -> String {
        self.line_name(sid)
    }

    /// The one-TS ini line of `tsid` (`BS15/TS1=1318000,0x40F1`).
    pub fn ts_line(&self, tsid: u16) -> String {
        format!("{}={},0x{tsid:04X}", self.ts_name(tsid), self.if_khz)
    }

    /// The one-service ini line of `sid` on `tsid` (`BS15/101=1318000,0x40F1,101`).
    pub fn service_line(&self, tsid: u16, sid: u16) -> String {
        format!(
            "{}={},0x{tsid:04X},{sid}",
            self.service_name(sid),
            self.if_khz
        )
    }
}

/// The transponder of a satellite TS: BS (network 4, TSID 0x4xxx) or CS110 (networks
/// 6 and 7, TSID 0x6xxx / 0x7xxx), bits 4-8 of the TSID the BS transponder or ND
/// number; `None` for any other (terrestrial TSIDs are 0x7xxx too) and for a left-hand
/// one.
pub fn transponder(onid: u16, tsid: u16) -> Option<Transponder> {
    let num = (tsid >> 4) & 0x1F;
    if onid == ONID_BS && tsid >> 12 == 0x4 {
        return bs_if_khz(num).map(|if_khz| Transponder {
            bs: true,
            num,
            if_khz,
        });
    }
    let cs = ONID_CS.contains(&onid) && matches!(tsid >> 12, 0x6 | 0x7);
    cs.then(|| nd_if_khz(num))
        .flatten()
        .map(|if_khz| Transponder {
            bs: false,
            num,
            if_khz,
        })
}

/// The relative TS number of a satellite TSID (its low 4 bits).
pub fn rel_ts(tsid: u16) -> u16 {
    tsid & 0xF
}

/// The value of a `[BS]` / `[CS110]` ini line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SatLine {
    /// The IF in Hz.
    pub freq_hz: u32,
    /// [`NO_TSID`] when the line has none.
    pub tsid: u16,
    /// `None` when omitted or negative (every service of the TS).
    pub sid: Option<u16>,
}

/// `IF_kHz,TSID[,ServiceID]`; `None` for an IF outside 950-2150 MHz.
pub fn satellite_line(value: &str) -> Option<SatLine> {
    let (khz, rest) = strtoul(value);
    let mut tsid = u32::from(NO_TSID);
    let mut sid: i32 = -1;
    if let Some(rest) = rest.strip_prefix(',') {
        let (t, rest) = strtoul(rest);
        tsid = t;
        if let Some(rest) = rest.strip_prefix(',') {
            sid = strtol(rest).0;
        }
    }
    SAT_IF_KHZ.contains(&khz).then(|| SatLine {
        freq_hz: khz * 1000,
        tsid: tsid as u16, // truncated to 16 bits
        // the device takes a 16-bit service id
        sid: (sid >= 0).then_some((sid & 0xFFFF) as u16),
    })
}

/// One channel of a tuning space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub name: String,
    pub sys: System,
    pub freq_hz: u32,
    /// ISDB-S only; [`NO_TSID`] = none, up to [`MAX_TS_NUMBER`] = a TS number.
    pub tsid: u16,
    /// The ini's ServiceID: stream that service only; `None` = every service of the TS.
    pub sid: Option<u16>,
    /// A number a channel file lists but that is no channel to tune, and why; `None` =
    /// a channel.
    pub untunable: Option<String>,
}

impl Channel {
    /// Whether the TSID is a TS number ([`MAX_TS_NUMBER`]).
    pub fn ts_number(&self) -> Option<u16> {
        (self.sys == System::Satellite && self.tsid <= MAX_TS_NUMBER).then_some(self.tsid)
    }
}

/// What a tuning space holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceKind {
    /// The built-in UHF 13-62ch.
    Uhf,
    /// The built-in CATV C13-C63.
    Catv,
    /// The `[BS]` lines.
    Bs,
    /// The `[CS110]` lines.
    Cs110,
    /// A space of the channel file of another BonDriver ([`file_spaces`]): any channels.
    File,
}

/// A tuning space (UHF, CATV, BS, CS110, or one of a channel file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Space {
    pub name: String,
    pub kind: SpaceKind,
    pub channels: Vec<Channel>,
}

fn terrestrial(name: String, freq_hz: u32) -> Channel {
    Channel {
        name,
        sys: System::Terrestrial,
        freq_hz,
        tsid: NO_TSID,
        sid: None, // every service of the TS
        untunable: None,
    }
}

/// UHF 13-62ch. ISDB-T channel centre = band centre + 1/7 MHz.
pub fn uhf_space() -> Space {
    Space {
        name: "UHF".into(),
        kind: SpaceKind::Uhf,
        channels: UHF
            .filter_map(|n| Some(terrestrial(format!("{n}ch"), uhf_hz(n)?)))
            .collect(),
    }
}

/// CATV C13-C63: C13-C21 at 108-162 MHz, C22 at 164-170 MHz, C23-C63 at 222-468 MHz.
pub fn catv_space() -> Space {
    Space {
        name: "CATV".into(),
        kind: SpaceKind::Catv,
        channels: CATV
            .filter_map(|n| Some(terrestrial(format!("C{n}ch"), catv_hz(n)?)))
            .collect(),
    }
}

/// One `name=freq_kHz,TSID[,ServiceID]` line of a `[BS]` / `[CS110]` section
/// ([`satellite_line`]). `None` for an empty name or a value that does not parse.
pub fn satellite_channel(name: &str, value: &str) -> Option<Channel> {
    if name.is_empty() {
        return None;
    }
    let line = satellite_line(value)?;
    Some(Channel {
        name: name.to_owned(),
        sys: System::Satellite,
        freq_hz: line.freq_hz,
        tsid: line.tsid,
        sid: line.sid,
        untunable: None,
    })
}

/// The tuning spaces of a `BonDriver_DT295.ini`: when `[Tuner] ChannelFile` names one
/// (`channel_file`), the spaces of that channel file of another BonDriver and no others
/// ([`file_spaces`]: numbered as that driver numbers them), else the ini's own
/// ([`spaces`]). With them, what to log about the channel file; `Err` when it cannot be
/// read.
pub fn ini_spaces(
    ini: &Ini,
    channel_file: Option<&Path>,
) -> Result<(Vec<Space>, Vec<String>), String> {
    let Some(path) = channel_file else {
        return Ok((spaces(ini), Vec::new()));
    };
    let file =
        dt295_chdef::load(path).map_err(|e| format!("ChannelFile {}: {e}", path.display()))?;
    let spaces = file_spaces(&file);
    let mut notes = vec![format!(
        "ChannelFile {}: {}, {} tuning spaces",
        path.display(),
        file.format,
        spaces.len()
    )];
    for (n, s) in spaces.iter().enumerate() {
        let untunable = s.channels.iter().filter(|c| c.untunable.is_some()).count();
        notes.push(format!(
            "  space {n} {}: {} channels{}",
            s.name,
            s.channels.len(),
            match untunable {
                0 => String::new(),
                u => format!(" ({u} not to tune)"),
            }
        ));
    }
    Ok((spaces, notes))
}

/// The tuning spaces of a channel file of another BonDriver, in its order. Its numbers
/// without a channel to tune stay as [`Channel::untunable`] ones, so the numbers do not
/// shift; a TS number is a TSID up to [`MAX_TS_NUMBER`]. The file's service ids are not
/// used: a channel streams its whole TS.
pub fn file_spaces(file: &ChannelFile) -> Vec<Space> {
    file.spaces
        .iter()
        .map(|s| Space {
            name: s.name.clone(),
            kind: SpaceKind::File,
            channels: s
                .channels
                .iter()
                .map(|c| {
                    let name = c.name.clone();
                    match &c.tuning {
                        Tuning::Terrestrial { hz } => terrestrial(name, *hz),
                        Tuning::Satellite { if_hz, ts } => Channel {
                            name,
                            sys: System::Satellite,
                            freq_hz: *if_hz,
                            tsid: match *ts {
                                Ts::First => NO_TSID,
                                Ts::Number(n) => u16::from(n),
                                Ts::Id(t) => t,
                            },
                            sid: None,
                            untunable: None,
                        },
                        Tuning::Untunable(why) => Channel {
                            untunable: Some(why.clone()),
                            ..terrestrial(name, 0)
                        },
                    }
                })
                .collect(),
        })
        .collect()
}

/// The tuning spaces of a `BonDriver_DT295.ini`, in the order the BonDriver lists
/// them (the space numbers of TVTest's `.ch2`): UHF and CATV unless `[Tuner] UHF` /
/// `CATV` turn them off (`GetPrivateProfileInt`, absent or empty = on), then `[BS]`
/// and `[CS110]` when they have lines that parse, in file order.
pub fn spaces(ini: &Ini) -> Vec<Space> {
    let mut out = Vec::new();
    if profile::get_int(ini, "Tuner", "UHF", 1) != 0 {
        out.push(uhf_space());
    }
    if profile::get_int(ini, "Tuner", "CATV", 1) != 0 {
        out.push(catv_space());
    }
    for (section, kind) in [("BS", SpaceKind::Bs), ("CS110", SpaceKind::Cs110)] {
        let channels: Vec<_> = profile::lines(ini, section)
            .into_iter()
            .filter_map(|(k, v)| satellite_channel(&k, v))
            .collect();
        if !channels.is_empty() {
            out.push(Space {
                name: section.to_owned(),
                kind,
                channels,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn satellite_plan_lines() {
        // every right-hand transponder makes a satellite line
        let all = (1..=23)
            .filter_map(bs_if_khz)
            .chain((2..=24).filter_map(nd_if_khz));
        let all: Vec<u32> = all.collect();
        assert_eq!(all.len(), 12 + 12);
        for khz in all {
            assert!(satellite_line(&format!("{khz},0x4010")).is_some());
        }
    }

    #[test]
    fn transponders_of_satellite_tsids() {
        let tp = |onid, tsid| transponder(onid, tsid).map(|t| (t.bs, t.num, t.if_khz));
        assert_eq!(tp(4, 0x40F1), Some((true, 15, 1_318_000)));
        assert_eq!(tp(4, 0x4030), Some((true, 3, 1_087_840)));
        assert_eq!(
            tp(4, 0x46D2),
            Some((true, 13, 1_279_640)),
            "bits 9-11 are not the number"
        );
        assert_eq!(tp(6, 0x6020), Some((false, 2, 1_613_000)));
        assert_eq!(tp(7, 0x7040), Some((false, 4, 1_653_000)));
        assert_eq!(tp(7, 0x7160), Some((false, 22, 2_013_000)));
        assert_eq!(tp(4, 0x4020), None, "BS-2: left-hand");
        assert_eq!(tp(7, 0x7030), None, "ND03: left-hand");
        // terrestrial TSIDs are 0x7xxx too (TOKYO MX: 0x7E87, bits 4-8 = 8)
        assert_eq!(tp(32391, 0x7E87), None);
        assert_eq!(tp(32736, 0x7FE0), None);
        assert_eq!(tp(6, 0x4010), None, "a BS TSID on a CS network");
    }

    #[test]
    fn transponder_lines() {
        let bs = transponder(4, 0x40F1).unwrap();
        assert_eq!(bs.ts_name(0x40F1), "BS15/TS1");
        assert_eq!(bs.service_name(101), "BS15/101");
        assert_eq!(bs.ts_line(0x40F1), "BS15/TS1=1318000,0x40F1");
        assert_eq!(bs.service_line(0x40F1, 101), "BS15/101=1318000,0x40F1,101");
        let nd = transponder(6, 0x6020).unwrap();
        assert_eq!(nd.ts_line(0x6020), "ND02/TS0=1613000,0x6020");
        assert_eq!(rel_ts(0x48F3), 3);
        // a line reads back as what it was made from
        let line = satellite_line("1318000,0x40F1,101").unwrap();
        assert_eq!(
            (line.freq_hz, line.tsid, line.sid),
            (bs.if_khz * 1000, 0x40F1, Some(101))
        );
    }

    #[test]
    fn satellite_lines() {
        let l = satellite_line("1318000,0x40F1,101").unwrap();
        assert_eq!(
            (l.freq_hz, l.tsid, l.sid),
            (1_318_000_000, 0x40F1, Some(101))
        );
        let l = satellite_line("1733000,0x6080").unwrap();
        assert_eq!((l.tsid, l.sid), (0x6080, None));
        assert_eq!(satellite_line("1733000,0x6080,-1").unwrap().sid, None);
        assert_eq!(satellite_line("1049480").unwrap().tsid, NO_TSID);
        assert!(satellite_line("949999,0x4010").is_none());
        assert!(satellite_line("2150001,0x4010").is_none());
    }

    #[test]
    fn uhf_table() {
        let s = uhf_space();
        assert_eq!((s.name.as_str(), s.kind), ("UHF", SpaceKind::Uhf));
        assert_eq!(s.channels.len(), 50);
        assert_eq!(s.channels[0].name, "13ch");
        assert_eq!(s.channels[0].freq_hz, 473_142_857);
        assert_eq!(s.channels[14].name, "27ch");
        assert_eq!(s.channels[14].freq_hz, 557_142_857);
        assert_eq!(s.channels[49].name, "62ch");
        assert_eq!(s.channels[49].freq_hz, 767_142_857);
        for c in &s.channels {
            assert_eq!(c.sys, System::Terrestrial);
            assert_eq!(c.tsid, NO_TSID);
            assert_eq!(c.sid, None);
        }
    }

    #[test]
    fn catv_table() {
        let s = catv_space();
        assert_eq!((s.name.as_str(), s.kind), ("CATV", SpaceKind::Catv));
        assert_eq!(s.channels.len(), 51);
        assert_eq!(s.channels[0].name, "C13ch");
        assert_eq!(s.channels[0].freq_hz, 111_142_857);
        assert_eq!(s.channels[9].name, "C22ch");
        assert_eq!(s.channels[9].freq_hz, 167_142_857);
        assert_eq!(s.channels[10].name, "C23ch");
        assert_eq!(s.channels[10].freq_hz, 225_142_857);
        assert_eq!(s.channels[50].name, "C63ch");
        assert_eq!(s.channels[50].freq_hz, 465_142_857);
    }

    #[test]
    fn satellite_channels() {
        let c = satellite_channel("BS01/TS0", "1049480,0x4010,151").unwrap();
        assert_eq!(c.sys, System::Satellite);
        assert_eq!(c.freq_hz, 1_049_480_000);
        assert_eq!(c.tsid, 0x4010);
        assert_eq!(c.sid, Some(151));

        let c = satellite_channel("ND08", "1733000,0x6080").unwrap();
        assert_eq!((c.tsid, c.sid), (0x6080, None));

        let c = satellite_channel("x", "1733000,0x6080,-1").unwrap();
        assert_eq!(c.sid, None);

        let c = satellite_channel("x", "1733000").unwrap();
        assert_eq!((c.tsid, c.sid), (NO_TSID, None));

        // Range check (HORUS3A 950-2150 MHz IF), empty name, junk.
        assert!(satellite_channel("x", "949999,0x4010").is_none());
        assert!(satellite_channel("x", "2150001,0x4010").is_none());
        assert!(satellite_channel("x", "950000,1").is_some());
        assert!(satellite_channel("x", "2150000,1").is_some());
        assert!(satellite_channel("", "1049480,0x4010").is_none());
        assert!(satellite_channel("x", "abc").is_none());
        assert!(satellite_channel("x", "").is_none());

        // TSID truncates to 16 bits; octal and spaces as wcstoul.
        let c = satellite_channel("x", "1049480,0x14010, 0x97").unwrap();
        assert_eq!((c.tsid, c.sid), (0x4010, Some(0x97)));
        let c = satellite_channel("x", " 1049480,040020,010").unwrap();
        assert_eq!((c.tsid, c.sid), (0x4010, Some(8)));
        // Something other than ',' after a number ends the line's fields.
        let c = satellite_channel("x", "1049480;0x4010,151").unwrap();
        assert_eq!((c.tsid, c.sid), (NO_TSID, None));
    }

    fn spaces_of(text: &str) -> Vec<Space> {
        spaces(&profile::parse(text).expect("parses"))
    }

    #[test]
    fn spaces_of_an_ini() {
        let s = spaces_of("");
        let names: Vec<_> = s.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["UHF", "CATV"], "no ini: the built-in spaces");

        let s = spaces_of(
            "[Tuner]\nUHF=0\nCATV=0\n\
             [BS]\n\
             ; BS-1 (11727.48 MHz): comment without '='\n\
             BS01/TS0=1049480,0x4010,151\n\
             BS01/TS1 = 1049480 , 0x4011\n\
             a stray line\n\
             =1049480,0x4012\n\
             J:COM BS=1471440,0x4973,260\n\
             back\\slash=1471440,0x4973\n\
             Out of range=100000,0x4010\n\
             [CS110]\n\
             ND02/296=1613000,0x6020,296 ; not a comment for Win32 either\n\
             [Empty]\n",
        );
        assert_eq!(s.len(), 2);
        let bs = &s[0];
        assert_eq!((bs.name.as_str(), bs.kind), ("BS", SpaceKind::Bs));
        let names: Vec<_> = bs.channels.iter().map(|ch| ch.name.as_str()).collect();
        assert_eq!(names, ["BS01/TS0", "BS01/TS1", "J:COM BS", "back\\slash"]);
        assert_eq!(bs.channels[0].sid, Some(151));
        // wcstoul stops at the blank before ',': TSID stays "none".
        assert_eq!(bs.channels[1].tsid, NO_TSID);
        assert_eq!(bs.channels[2].tsid, 0x4973);
        assert_eq!(bs.channels[2].sys, System::Satellite);
        let cs = &s[1];
        assert_eq!((cs.name.as_str(), cs.kind), ("CS110", SpaceKind::Cs110));
        assert_eq!(cs.channels.len(), 1);
        assert_eq!(cs.channels[0].sid, Some(296));
    }

    #[test]
    fn space_switches_and_sections() {
        let kinds =
            |text: &str| -> Vec<SpaceKind> { spaces_of(text).iter().map(|s| s.kind).collect() };
        use SpaceKind::*;
        // GetPrivateProfileInt: absent or empty = on, else the leading number.
        assert_eq!(kinds("[Tuner]\nUHF=\nCATV=\n"), [Uhf, Catv]);
        assert_eq!(kinds("[Tuner]\nUHF=0\nCATV=yes\n"), [] as [SpaceKind; 0]);
        assert_eq!(kinds("[Tuner]\nUHF=0x0\nCATV=2\n"), [Catv]);
        // A section without a line that parses is no space; a second [BS] is not read.
        assert_eq!(kinds("[BS]\nx=1\n[CS110]\n"), [Uhf, Catv]);
        let s = spaces_of(
            "[BS]\nA=1049480,0x4010\n[CS110]\nN=1613000,0x6020\n[BS]\nB=1087840,0x4030\n",
        );
        let names: Vec<_> = s[2].channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!((s.len(), names), (4, vec!["A"]));
    }

    #[test]
    fn channel_file_spaces() {
        let dir = std::env::temp_dir().join(format!("dt295-chan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("BonDriver_PT3-S.ChSet.txt");
        std::fs::write(
            &path,
            "$BS\t0\nBS01/TS1\t0\t0\t0\t1\nBS15/TS1\t0\t1\t7\t16625\nBS99\t0\t2\t99\t0\n",
        )
        .unwrap();
        let ini = profile::parse("[Tuner]\nUHF=1\n[BS]\nA=1049480,0x4010\n").unwrap();
        let (s, notes) = ini_spaces(&ini, Some(&path)).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(s.len(), 1, "the file's spaces only");
        assert_eq!((s[0].name.as_str(), s[0].kind), ("BS", SpaceKind::File));
        let c = &s[0].channels;
        assert_eq!(
            (c[0].sys, c[0].freq_hz, c[0].tsid, c[0].ts_number()),
            (System::Satellite, 1_049_480_000, 1, Some(1))
        );
        assert_eq!((c[1].tsid, c[1].ts_number()), (0x40F1, None));
        assert!(c[2].untunable.is_some(), "PTx channel 99");
        assert!(notes[0].contains("ChSet.txt, 1 tuning spaces"), "{notes:?}");
        assert!(notes[1].contains("3 channels (1 not to tune)"), "{notes:?}");

        assert!(ini_spaces(&ini, Some(&dir.join("absent.txt"))).is_err());
        let (own, notes) = ini_spaces(&ini, None).unwrap();
        assert_eq!((own.len(), notes.len()), (3, 0), "UHF, CATV, BS");
    }
}
