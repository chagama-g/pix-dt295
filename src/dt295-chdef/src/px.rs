//! The satellite ini of the BonDriver_PX series (PLEX PX-W3PE, PX-Q3PE and their kin:
//! `BonDriver_PX_W3PE_S0.ini`): the tuning spaces `[BS]` and `[CS110]`, in that order,
//! each with `CHANNEL_NUM = n` and `CH001` to `CHnnn`:
//!
//! `CH001 = name,service id,frequency (kHz),TSID`, e.g. `CH001 = BS1/TS0,151,11727480,0x4010`
//!
//! The frequency is the transponder's (11727.48 MHz for BS-1), from which the LNB's local
//! oscillator (10678 MHz) is taken; one already in the tuner's IF range is taken as the
//! IF. The service id is the one the channel shows first; the whole TS streams. A TSID of
//! 7 or less (`0x0` in the CS110 lines) is a TS number. Without `CHANNEL_NUM` the
//! highest `CHnnn` gives the count; a number without a line is listed as `----`.

use crate::num::strtoul;
use crate::plan::{LNB_LO_KHZ, SAT_IF_KHZ};
use crate::profile::{self, Ini};
use crate::{Channel, Space, Ts, Tuning};

const SECTIONS: [&str; 2] = ["BS", "CS110"];

/// `CHnnn`: its number.
fn ch_number(key: &str) -> Option<u32> {
    let digits = key
        .get(..2)?
        .eq_ignore_ascii_case("CH")
        .then(|| &key[2..])?;
    (digits.len() == 3 && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// Whether `[BS]` or `[CS110]` has a `CHANNEL_NUM`, or a `CHnnn` line of 3 or more fields.
pub fn is_px(ini: &Ini) -> bool {
    SECTIONS.iter().any(|s| {
        profile::lines(ini, s).iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("CHANNEL_NUM")
                || (ch_number(k).is_some() && v.split(',').count() >= 3)
        })
    })
}

/// The tuning spaces of a BonDriver_PX ini: `[BS]` and `[CS110]` when they have lines.
pub fn parse(ini: &Ini) -> Vec<Space> {
    SECTIONS
        .iter()
        .filter_map(|&section| {
            let lines = profile::lines(ini, section);
            if lines.is_empty() {
                return None;
            }
            let count = match profile::get(ini, section, "CHANNEL_NUM") {
                Some(n) if !n.trim().is_empty() => strtoul(n).0,
                _ => lines
                    .iter()
                    .filter_map(|(k, _)| ch_number(k))
                    .max()
                    .unwrap_or(0),
            };
            let channels = (1..=count.min(999))
                .map(|n| match profile::get(ini, section, &format!("CH{n:03}")) {
                    Some(value) => channel(value),
                    None => crate::undefined("----"),
                })
                .collect();
            Some(Space {
                name: section.to_owned(),
                channels,
            })
        })
        .collect()
}

/// `name,service id,frequency,TSID`.
fn channel(value: &str) -> Channel {
    let mut f = value.split(',').map(str::trim);
    let name = f.next().unwrap_or("").to_owned();
    let _service = f.next();
    let khz = strtoul(f.next().unwrap_or("")).0;
    let ts = match f.next() {
        None | Some("") => Ts::First,
        Some(t) => Ts::from_field(strtoul(t).0),
    };
    let if_khz = if SAT_IF_KHZ.contains(&khz) {
        i64::from(khz)
    } else {
        i64::from(khz) - i64::from(LNB_LO_KHZ)
    };
    let tuning = if khz == 0 {
        Tuning::Untunable("no frequency".into())
    } else {
        crate::satellite(if_khz, ts)
    };
    Channel { name, tuning }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sat(khz: u32, ts: Ts) -> Tuning {
        Tuning::Satellite {
            if_hz: khz * 1000,
            ts,
        }
    }

    const INI: &str = "[BS]\r\n\
        CHANNEL_NUM = 4\r\n\
        CH001 = BS1/TS0,151,11727480,0x4010\r\n\
        CH002 = BS1/TS1,161,11727480,0x4011,BS-TBS\r\n\
        CH004 = BS15/TS1,0,1318000,0x40F1\r\n\
        CH005 = BS23/TS2,0,12149440,0x4973\r\n\
        [CS110]\r\n\
        CHANNEL_NUM\t= 2\r\n\
        CH001\t= ND2,237,12291000,0x0\r\n\
        CH002\t= ND4,100,12331000,0x0\r\n";

    #[test]
    fn spaces_and_channels() {
        let ini = profile::parse(INI).unwrap();
        assert!(is_px(&ini));
        let s = parse(&ini);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "BS");
        let bs: Vec<_> = s[0].channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            bs,
            ["BS1/TS0", "BS1/TS1", "----", "BS15/TS1"],
            "CHANNEL_NUM counts"
        );
        assert_eq!(s[0].channels[0].tuning, sat(1_049_480, Ts::Id(0x4010)));
        assert_eq!(s[0].channels[1].tuning, sat(1_049_480, Ts::Id(0x4011)));
        assert!(matches!(s[0].channels[2].tuning, Tuning::Untunable(_)));
        assert_eq!(
            s[0].channels[3].tuning,
            sat(1_318_000, Ts::Id(0x40F1)),
            "an IF as the IF"
        );
        assert_eq!(s[1].name, "CS110");
        assert_eq!(s[1].channels[1].tuning, sat(1_653_000, Ts::Number(0)));
    }

    #[test]
    fn without_channel_num() {
        let ini = profile::parse("[CS110]\nCH002 = ND4,100,12331000,0x0\n").unwrap();
        assert!(is_px(&ini));
        let s = parse(&ini);
        assert_eq!(s.len(), 1, "no [BS]");
        let names: Vec<_> = s[0].channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["----", "ND4"]);
        // BonDriver_DT295's own lines are no PX lines
        let own = profile::parse("[BS]\nBS01/TS0=1049480,0x4010\n").unwrap();
        assert!(!is_px(&own));
    }

    #[test]
    fn without_tsid() {
        // no TSID (or an empty one): the first TS, not TS number 0
        for line in ["BS15,101,11996000", "BS15,101,11996000, "] {
            let ini = profile::parse(&format!("[BS]\nCH001 = {line}\n")).unwrap();
            assert_eq!(parse(&ini)[0].channels[0].tuning, sat(1_318_000, Ts::First));
        }
    }

    #[test]
    fn keys() {
        assert_eq!(ch_number("CH001"), Some(1));
        assert_eq!(ch_number("ch030"), Some(30));
        assert_eq!(ch_number("CH1"), None);
        assert_eq!(ch_number("CHANNEL_NUM"), None);
        assert_eq!(ch_number("C"), None);
    }
}
