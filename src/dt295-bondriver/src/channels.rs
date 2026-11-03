//! Tuning spaces and channels: the built-in ISDB-T tables (UHF 13-62ch, CATV
//! C13-C63) and the satellite lines of the ini (`name=IF_kHz,TSID[,ServiceID]`).

use dt295::System;
use dt295::chan;

pub use dt295::chan::{NO_TSID, strtol, strtoul};

/// One channel of a tuning space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub name: String,
    pub sys: System,
    pub freq_hz: u32,
    /// ISDB-S only; [`NO_TSID`] = none.
    pub tsid: u16,
    /// The ini's ServiceID: stream that service only; `None` = every service of the TS.
    pub sid: Option<u16>,
}

/// A tuning space (UHF, CATV, BS, CS110).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Space {
    pub name: String,
    pub channels: Vec<Channel>,
}

fn terrestrial(name: String, freq_hz: u32) -> Channel {
    Channel {
        name,
        sys: System::Terrestrial,
        freq_hz,
        tsid: NO_TSID,
        sid: None, // every service of the TS
    }
}

/// UHF 13-62ch. ISDB-T channel centre = band centre + 1/7 MHz.
pub fn uhf_space() -> Space {
    Space {
        name: "UHF".into(),
        channels: chan::UHF
            .filter_map(|n| Some(terrestrial(format!("{n}ch"), chan::uhf_hz(n)?)))
            .collect(),
    }
}

/// CATV C13-C63: C13-C21 at 108-162 MHz, C22 at 164-170 MHz, C23-C63 at 222-468 MHz.
pub fn catv_space() -> Space {
    Space {
        name: "CATV".into(),
        channels: chan::CATV
            .filter_map(|n| Some(terrestrial(format!("C{n}ch"), chan::catv_hz(n)?)))
            .collect(),
    }
}

/// One `name=freq_kHz,TSID[,ServiceID]` line of a `[BS]` / `[CS110]` section, with
/// `wcstoul(.., 0)` number syntax (decimal, `0x` hex, `0` octal).
/// ServiceID omitted or negative = every service. `None` for an empty name or an IF outside
/// the HORUS3A range (950-2150 MHz).
pub fn satellite_channel(name: &str, value: &str) -> Option<Channel> {
    if name.is_empty() {
        return None;
    }
    let line = chan::satellite_line(value)?;
    Some(Channel {
        name: name.to_owned(),
        sys: System::Satellite,
        freq_hz: line.freq_hz,
        tsid: line.tsid,
        sid: line.sid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uhf_table() {
        let s = uhf_space();
        assert_eq!(s.name, "UHF");
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
        assert_eq!(s.name, "CATV");
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
    fn satellite_lines() {
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

    #[test]
    fn c_number_parsing() {
        assert_eq!(strtoul("123abc"), (123, "abc"));
        assert_eq!(strtoul("  0x1F,"), (0x1F, ","));
        assert_eq!(strtoul("0X1f"), (0x1F, ""));
        assert_eq!(strtoul("017"), (15, ""));
        assert_eq!(strtoul("0"), (0, ""));
        assert_eq!(strtoul("0x"), (0, "x"));
        assert_eq!(strtoul("08"), (0, "8"));
        assert_eq!(strtoul("-1"), (u32::MAX, ""));
        assert_eq!(strtoul("99999999999"), (u32::MAX, ""));
        assert_eq!(strtoul("-99999999999"), (u32::MAX, ""));
        assert_eq!(strtoul(",5"), (0, ",5"));
        assert_eq!(strtoul("  x"), (0, "  x"));
        assert_eq!(strtol("-1"), (-1, ""));
        assert_eq!(strtol("+42 "), (42, " "));
        assert_eq!(strtol("99999999999"), (i32::MAX, ""));
        assert_eq!(strtol("-99999999999"), (i32::MIN, ""));
        assert_eq!(strtol("z"), (0, "z"));
    }
}
