//! `<name>.ChSet.txt`, the channel file of BonDriver_PT-ST / PT3-ST and their forks
//! (BonDriver_PTx-ST_mod, px4_drv's BonDriver_PX4), read as BonDriver_PTx-ST_mod reads
//! it. A file of BonDriver_PX4 reads the same when no channel is given twice
//! (BonDriver_PX4 takes the last one) and its `$` lines go 0, 1, 2 ... (it numbers the
//! spaces in file order); the `%` lines, `-`, `+`, the offsets and TS numbers are
//! BonDriver_PTx-ST_mod's. One definition per line, its fields separated by tabs; a line
//! starting with `;` or without a tab is none:
//!
//! - `$name<TAB>space`: a tuning space
//! - `%name<TAB>space<TAB>channel<TAB>PTx channel`: a transponder (BonDriver_PTx-ST_mod;
//!   no channel, but the next line's `-` and `+` refer to it)
//! - `name<TAB>space<TAB>channel<TAB>PTx channel<TAB>TSID`: a channel
//!
//! Numbers are decimal (C `atoi`); `-` is the value of the line before, `+` that plus 1.
//! A PTx channel may carry a frequency offset, `36-1` or `36+14` (BonDriver_PTx-ST_mod):
//! 1/7 MHz steps for ISDB-T, 1 MHz for ISDB-S. A TSID of 7 or less is a TS number
//! ([`Ts::Number`](crate::Ts::Number)). Of a space or channel number given twice the
//! first line counts; the driver lists the spaces from 0 up to the first number without
//! one, and so the channels of a space.
//!
//! The PTx channels of a file for ISDB-S are BS-1 to BS-23 (0-11), ND02 to ND24 (12-23)
//! and ND01 to ND23 (24-35, left-hand, so none here), those of a file for ISDB-T VHF 1-3
//! (0-2), C13-C22 (3-12),
//! VHF 4-12 (13-21), C23-C63 (22-62) and UHF 13-62 (63-112). Which one a file is, the PT
//! driver takes from its DLL's name (`BonDriver_PT3-T.dll` is for ISDB-T); here the
//! file's name tells it the same way ([`is_satellite`]).

use std::collections::BTreeMap;

use crate::plan::{bs_if_khz, catv_hz, nd_if_khz, uhf_hz};
use crate::{Channel, Space, Ts, Tuning};

/// Whether `text` has a tuning space line (`$name<TAB>n`), which a ChSet.txt needs.
pub fn is_chset(text: &str) -> bool {
    text.lines().any(|l| l.starts_with('$') && l.contains('\t'))
}

/// Whether a ChSet.txt is for ISDB-S, by its file name as the PT driver tells it by its
/// DLL's: the letter after the `-` of `BonDriver_PT3-` (`PTx`, `PTw`, `PT`, ...; the
/// name's first `-`) is `T` for ISDB-T (`BonDriver_PT3-T1`, `BonDriver_PT3-T-room`),
/// anything else, or no `-`, is ISDB-S.
pub fn is_satellite(file_name: &str) -> bool {
    let stem = file_name.split('.').next().unwrap_or("");
    let after = stem.find('-').map(|i| &stem[i + 1..]).unwrap_or("");
    !after.starts_with(['T', 't'])
}

/// The values of a line, and of the one before (`-`, `+`).
#[derive(Debug, Clone, Copy, Default)]
struct Line {
    space: u32,
    channel: u32,
    /// The PTx channel (bits 15-0) and its offset (bits 31-16, signed).
    ptx: u32,
    tsid: u32,
}

/// The tuning spaces of a ChSet.txt ([`is_satellite`]: for ISDB-S).
pub fn parse(text: &str, satellite: bool) -> Vec<Space> {
    let mut spaces: BTreeMap<u32, String> = BTreeMap::new();
    let mut channels: BTreeMap<(u32, u32), (String, Line)> = BTreeMap::new();
    let mut last = Line::default();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with(';') || !line.contains('\t') {
            continue;
        }
        if line.starts_with('$') {
            let line = line.replace('$', "");
            let mut f = line.split('\t');
            let name = f.next().unwrap_or("").to_owned();
            let space = decimal(f.next().unwrap_or(""));
            spaces.entry(space).or_insert(name);
        } else if line.starts_with('%') {
            // a transponder: only the values the next line refers to
            let line = line.replace('%', "");
            let mut f = line.split('\t').skip(1);
            let mut field = || f.next().unwrap_or("");
            last = Line {
                space: data(field(), last.space),
                channel: data(field(), last.channel),
                ptx: ptx_channel(field(), last.ptx),
                tsid: 0,
            };
        } else {
            let mut f = line.split('\t');
            let name = f.next().unwrap_or("").to_owned();
            let mut field = || f.next().unwrap_or("");
            let this = Line {
                space: data(field(), last.space),
                channel: data(field(), last.channel),
                ptx: ptx_channel(field(), last.ptx),
                tsid: data(field(), last.tsid) & 0xFFFF,
            };
            last = this;
            channels
                .entry((this.space, this.channel))
                .or_insert((name, this));
        }
    }
    (0..)
        .map_while(|s| {
            let name = spaces.get(&s)?.clone();
            let channels = (0..)
                .map_while(|c| channels.get(&(s, c)))
                .map(|(name, line)| Channel {
                    name: name.clone(),
                    tuning: tuning(line, satellite),
                })
                .collect();
            Some(Space { name, channels })
        })
        .collect()
}

/// What a channel line tunes.
fn tuning(line: &Line, satellite: bool) -> Tuning {
    let ch = line.ptx & 0xFFFF;
    let offset = i64::from((line.ptx >> 16) as u16 as i16);
    if satellite {
        let if_khz = match ch {
            0..=11 => bs_if_khz(ch as u16 * 2 + 1),
            12..=23 => nd_if_khz((ch as u16 - 12) * 2 + 2),
            24..=35 => {
                let nd = (ch - 24) * 2 + 1;
                return Tuning::Untunable(format!("ND{nd:02} is left-hand"));
            }
            _ => None,
        };
        let Some(if_khz) = if_khz else {
            return Tuning::Untunable(format!("PTx channel {ch} is none for ISDB-S"));
        };
        crate::satellite(i64::from(if_khz) + offset * 1000, Ts::from_field(line.tsid))
    } else {
        // VHF centres: 1-3 at 93-105 MHz, 4-7 at 173-191 MHz, 8-12 at 195-219 MHz
        let vhf = |n: u32| {
            let mhz = match n {
                1..=3 => 93 + 6 * (n - 1),
                4..=7 => 173 + 6 * (n - 4),
                _ => 195 + 6 * (n - 8),
            };
            mhz * 1_000_000 + 142_857
        };
        let hz = match ch {
            0..=2 => Some(vhf(ch + 1)),
            3..=12 => catv_hz(ch + 10),
            13..=21 => Some(vhf(ch - 9)),
            22..=62 => catv_hz(ch + 1),
            63..=112 => uhf_hz(ch - 50),
            _ => None,
        };
        let Some(hz) = hz else {
            return Tuning::Untunable(format!("PTx channel {ch} is none for ISDB-T"));
        };
        // 1/7 MHz steps
        let hz = i64::from(hz) + offset * 1_000_000 / 7;
        match u32::try_from(hz) {
            Ok(hz) => Tuning::Terrestrial { hz },
            Err(_) => Tuning::Untunable(format!("PTx channel {ch} with offset {offset}")),
        }
    }
}

/// C `atoi`: leading blanks, a sign, decimal digits up to the first other character (none
/// = 0), modulo 2^32.
fn decimal(s: &str) -> u32 {
    let t = s.trim_start_matches([' ', '\t']);
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let v = t
        .chars()
        .map_while(|c| c.to_digit(10))
        .fold(0u32, |acc, d| acc.wrapping_mul(10).wrapping_add(d));
    if neg { v.wrapping_neg() } else { v }
}

/// A number field: `-` = the value before, `+` = it plus 1, else `atoi`.
fn data(s: &str, last: u32) -> u32 {
    match s {
        "-" => last,
        "+" => last.wrapping_add(1),
        _ => decimal(s),
    }
}

/// A PTx channel field with its offset (BonDriver_PTx-ST_mod's `ParsePT1Ch`): a leading
/// `-` or `+` starts from the channel before (and its offset; `+` is the next channel),
/// then `ch+n` / `ch-n` adds or takes an offset, and a bare number is the channel. The
/// result is the channel in bits 15-0, the offset in bits 31-16.
fn ptx_channel(s: &str, last: u32) -> u32 {
    let (mut ch, mut offset, rest) = match s.as_bytes().first() {
        Some(&b) if b == b'-' || b == b'+' => {
            let ch = (last & 0xFFFF) + u32::from(b == b'+');
            (ch, i32::from((last >> 16) as u16 as i16), &s[1..])
        }
        _ => (0, 0, s),
    };
    if let Some((prefix, suffix)) = rest.split_once('+').filter(|(_, s)| !s.is_empty()) {
        if !prefix.is_empty() {
            ch = decimal(prefix);
        }
        offset = offset.wrapping_add(decimal(suffix) as i32);
    } else if let Some((prefix, suffix)) = rest.split_once('-').filter(|(_, s)| !s.is_empty()) {
        if !prefix.is_empty() {
            ch = decimal(prefix);
        }
        offset = offset.wrapping_sub(decimal(suffix) as i32);
    } else if !rest.is_empty() {
        ch = decimal(rest);
    }
    (ch & 0xFFFF) | (u32::from(offset as u16) << 16)
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

    fn names(s: &Space) -> Vec<&str> {
        s.channels.iter().map(|c| c.name.as_str()).collect()
    }

    // In the form of BonDriver_PTx-ST_mod's sample (BS Almighty): transponder lines and
    // TS numbers, `-` and `+` throughout.
    const SATELLITE: &str = "; BS/CS\r\n\
        $BS\t0\r\n\
        $CS110\t1\r\n\
        %BS01\t0\t0\t0\r\n\
        %BS03\t-\t+\t1\r\n\
        %ND02\t1\t0\t12\r\n\
        BS01/TS0\t0\t0\t0\t0\r\n\
        BS01/TS1\t-\t+\t-\t1\r\n\
        BS01/TS3\t-\t+\t-\t+\r\n\
        BS15/TS1\t-\t+\t7\t16625\r\n\
        BS23/TS2\t0\t4\t11\t18803\tＢＳ松竹東急\r\n\
        ND02\t1\t0\t12\t0\r\n\
        ND04\t-\t+\t+\t0\r\n\
        ND24\t1\t2\t23\t0\r\n\
        ND01\t1\t3\t24\t0\r\n\
        ND23\t1\t4\t35\t0\r\n";

    #[test]
    fn satellite_file() {
        assert!(is_chset(SATELLITE));
        let s = parse(SATELLITE, true);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "BS");
        assert_eq!(
            names(&s[0]),
            ["BS01/TS0", "BS01/TS1", "BS01/TS3", "BS15/TS1", "BS23/TS2"]
        );
        let t: Vec<_> = s[0].channels.iter().map(|c| c.tuning.clone()).collect();
        assert_eq!(
            t,
            [
                sat(1_049_480, Ts::Number(0)),
                sat(1_049_480, Ts::Number(1)),
                sat(1_049_480, Ts::Number(2)),
                sat(1_318_000, Ts::Id(0x40F1)),
                sat(1_471_440, Ts::Id(0x4973)),
            ]
        );
        assert_eq!(s[1].name, "CS110");
        assert_eq!(names(&s[1]), ["ND02", "ND04", "ND24", "ND01", "ND23"]);
        assert_eq!(s[1].channels[1].tuning, sat(1_653_000, Ts::Number(0)));
        assert_eq!(s[1].channels[2].tuning, sat(2_053_000, Ts::Number(0)));
        // the odd NDs are left-hand
        assert_eq!(
            s[1].channels[3].tuning,
            Tuning::Untunable("ND01 is left-hand".into())
        );
        assert_eq!(
            s[1].channels[4].tuning,
            Tuning::Untunable("ND23 is left-hand".into())
        );
    }

    #[test]
    fn terrestrial_file() {
        let text = "$地デジ(UHF)\t0\n;$地デジ(CATV)\t1\n$VHF\t1\n\
            13ch\t0\t0\t63\t0\n\
            14Ch\t0\t1\t64\t0\n\
            62ch\t0\t2\t112\t0\n\
            C13ch\t0\t3\t3\t0\n\
            C22ch\t0\t4\t12\t0\n\
            C37ch\t0\t5\t36-1\t0\n\
            1ch\t1\t0\t0\t0\n\
            4ch\t1\t1\t13\t0\n\
            12ch\t1\t2\t21\t0\n";
        let s = parse(text, false);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "地デジ(UHF)");
        let hz: Vec<_> = s
            .iter()
            .flat_map(|s| &s.channels)
            .map(|c| match c.tuning {
                Tuning::Terrestrial { hz } => hz,
                _ => 0,
            })
            .collect();
        assert_eq!(
            hz,
            [
                473_142_857,
                479_142_857,
                767_142_857,
                111_142_857,
                167_142_857,
                309_000_000, // C37, 1/7 MHz down
                93_142_857,
                173_142_857,
                219_142_857,
            ]
        );
    }

    #[test]
    fn numbering() {
        // a gap ends the list; the first of a number counts; a channel of a space that
        // is not listed is none
        let text = "$A\t0\n$C\t2\n$A2\t0\n\
            a0\t0\t0\t0\t0\n\
            a0 again\t0\t0\t1\t0\n\
            a1\t0\t1\t1\t0\n\
            a3\t0\t3\t3\t0\n\
            b0\t1\t0\t0\t0\n";
        let s = parse(text, true);
        assert_eq!(s.len(), 1, "no space 1");
        assert_eq!(s[0].name, "A");
        assert_eq!(names(&s[0]), ["a0", "a1"]);
        // LF line ends, a line without a tab, a comment
        let s = parse("$S\t0\nno tab\n;x\t0\t0\t0\t0\nx\t0\t0\t0\t0", true);
        assert_eq!(names(&s[0]), ["x"]);
    }

    #[test]
    fn ptx_channels_and_offsets() {
        assert_eq!(ptx_channel("7", 0), 7);
        assert_eq!(ptx_channel("-", 7), 7);
        assert_eq!(ptx_channel("+", 7), 8);
        assert_eq!(ptx_channel("36-1", 0), 36 | 0xFFFF << 16);
        assert_eq!(ptx_channel("36+14", 0), 36 | 14 << 16);
        // `+` carries the offset of the channel before
        assert_eq!(ptx_channel("+", 36 | 0xFFFF << 16), 37 | 0xFFFF << 16);
        assert_eq!(ptx_channel("+-1", 5), 6 | 0xFFFF << 16);
        assert_eq!(
            ptx_channel("+2", 5),
            2,
            "a number after the sign is the channel"
        );
        assert_eq!(ptx_channel("", 5), 0);
        // ISDB-S: 1 MHz steps
        let line = Line {
            ptx: ptx_channel("7+2", 0),
            ..Line::default()
        };
        assert_eq!(tuning(&line, true), sat(1_320_000, Ts::Number(0)));
        let line = Line {
            ptx: 36,
            ..Line::default()
        };
        assert!(matches!(tuning(&line, true), Tuning::Untunable(_)));
        assert!(matches!(
            tuning(&Line { ptx: 113, ..line }, false),
            Tuning::Untunable(_)
        ));
    }

    #[test]
    fn numbers() {
        assert_eq!(data("12", 0), 12);
        assert_eq!(data(" 12x", 0), 12);
        assert_eq!(data("0x4010", 0), 0, "atoi: decimal only");
        assert_eq!(data("", 9), 0);
        assert_eq!(data("-1", 0), u32::MAX);
    }

    #[test]
    fn file_names() {
        assert!(is_satellite("BonDriver_PT3-S.ChSet.txt"));
        assert!(is_satellite("BonDriver_PX4-S1.ChSet.txt"));
        assert!(is_satellite("channels.ChSet.txt"), "no -: ISDB-S");
        assert!(!is_satellite("BonDriver_PT3-T.ChSet.txt"));
        assert!(!is_satellite("BonDriver_PTx-T1.ChSet.txt"));
        assert!(!is_satellite("bondriver_pt-t.chset.txt"));
        assert!(is_satellite("BonDriver_PT-ST.ChSet.txt"), "-S first");
        // the letter after the first -, as BonDriver_PTx-ST_mod reads its DLL's name
        assert!(!is_satellite("BonDriver_PT3-T-room.ChSet.txt"));
        assert!(is_satellite("BonDriver_PT3-S-T.ChSet.txt"));
    }
}
