//! `BonDriver_BDA.ini`, read as BonDriver_BDA (radi-sh) reads it, for its tuning spaces:
//!
//! - `[TuningSpace00]` to `[TuningSpace99]`: a space each, numbered as the section;
//!   without `[TuningSpace00]`, `[Channel]` is space 0, which is there in any case. A
//!   number without a section is a space `-` without channels.
//! - In a space: `TuningSpaceName`, `CH000` to `CH999` =
//!   `satellite,frequency (MHz),polarisation,modulation[,name[,SID[,TSID[,ONID]]]]`
//!   (quotes around it removed), numbered in the order of their keys (`CH`, then
//!   `CH000` up: BonDriver_BDA keeps a section's lines in a map by key) unless
//!   `ReserveUnusedCh=YES` (then by their number, a number without a line listed as
//!   `----`), `ChannelSettingsAuto` (`UHF`, `CATV`, `BS`,
//!   `CS110`, ...) with `ChannelSettingsAutoOptions`, which stand for `ChannelGenerate0`,
//!   `ChannelGenerate1` ... lines (also given as such), and `FrequencyOffset` (kHz).
//!   Generated channels number from 0, and a `CHnnn` line then replaces channel nnn.
//! - `[Tuner] DefaultNetwork`, `[Satellite]` (the LNB per satellite and polarisation) and
//!   `[Modulation]` (each modulation number's kind) as the channels refer to them.
//!
//! Every key but `[Channel] ReserveUnusedCh` is read as BonDriver_BDA reads a section's
//! lines ([`string`]), not through the profile API. A channel whose satellite or
//! modulation number is past the tables (a `ChannelGenerate` line can give any) is
//! listed, but does not tune.
//!
//! A channel tunes as ISDB-T or ISDB-S when its modulation is that (`ISDB-T`,
//! `BDA_MOD_ISDB_T_TMCC`, ...), at its frequency plus the space's offset, minus the
//! satellite's local oscillator for ISDB-S. A TSID of 7 or less is a TS number (the
//! TSIDs `ChannelGenerate` makes with a relative TS count, and the `0x0` of the CS110
//! sample); none is the first TS. The SID is not used: the whole TS streams.

use std::collections::BTreeMap;

use crate::num::strtol;
use crate::plan::LNB_LO_KHZ;
use crate::profile::{self, Ini};
use crate::{Channel, Space, Ts, Tuning};

const MAX_SATELLITE: usize = 10;
const MAX_MODULATION: usize = 10;
/// The channel numbers a space keeps. BonDriver_BDA has no limit, but a number this high
/// is a mistake in the file (a `ChannelGenerate` start, a relative TS count), and listing
/// every channel up to it would take the host's memory.
const MAX_CHANNELS: u32 = 10_000;
/// ' ' (not set), H, V, L, R
const POLARISATIONS: [char; 5] = [' ', 'H', 'V', 'L', 'R'];

/// `[Tuner] DefaultNetwork`: which satellites and modulations are set up without being
/// given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Network {
    None,
    Sphd,
    BsCs,
    Uhf,
    Dual,
}

/// What a modulation number is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    IsdbT,
    IsdbS,
    /// Another one (DVB-S, QAM ...) or none: its name.
    Other(String),
}

/// The LNB of a satellite for one polarisation (kHz; 0 = not set).
#[derive(Debug, Clone, Copy, Default)]
struct Lnb {
    low: i64,
    high: i64,
    switch: i64,
}

impl Lnb {
    /// The local oscillator for a transponder at `khz`.
    fn oscillator(&self, khz: i64) -> i64 {
        if self.switch > 0 && khz >= self.switch {
            self.high
        } else {
            self.low
        }
    }
}

/// One channel as BonDriver_BDA keeps it.
#[derive(Debug, Clone)]
struct ChData {
    name: String,
    satellite: usize,
    polarisation: usize,
    modulation: usize,
    /// kHz
    frequency: i64,
    /// -1 = none
    tsid: i64,
}

impl Default for ChData {
    fn default() -> Self {
        Self {
            name: String::new(),
            satellite: 0,
            polarisation: 0,
            modulation: 0,
            frequency: 0,
            tsid: -1,
        }
    }
}

/// The whole-file settings the spaces refer to.
#[derive(Debug)]
struct Setup {
    satellite_names: Vec<String>,
    lnb: Vec<[Lnb; 5]>,
    kinds: Vec<Kind>,
    modulation_names: Vec<String>,
    /// The modulation numbers ChannelSettingsAuto uses.
    isdb_t: usize,
    isdb_s: usize,
    dvb_s: usize,
    dvb_s2: usize,
    qam64: usize,
    qam256: usize,
    opticast: usize,
    /// The satellite numbers ChannelSettingsAuto uses.
    bs_cs: usize,
    jcsat3: usize,
    jcsat4: usize,
}

/// Whether the ini has a `[TuningSpaceNN]`, or a `[Channel]` with channels.
pub fn is_bda(ini: &Ini) -> bool {
    let channel = profile::lines(ini, "Channel");
    (0..100).any(|n| !profile::lines(ini, &format!("TuningSpace{n:02}")).is_empty())
        || channel.iter().any(|(k, _)| {
            is_ch_key(k)
                || k.eq_ignore_ascii_case("ChannelSettingsAuto")
                || k.to_ascii_lowercase().starts_with("channelgenerate")
        })
}

/// `CHnnn` (or `CH`), the keys of channel lines.
fn is_ch_key(k: &str) -> bool {
    k.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("CH"))
        && matches!(k.len(), 2 | 5)
        && k.bytes().skip(2).all(|b| b.is_ascii_digit())
}

/// A key's value as `ReadKeySSectionData` gives it: the section's first line of the key
/// (compared case-insensitively), as BonDriver_BDA keeps a section's lines
/// ([`section_value`]).
fn string<'a>(ini: &'a Ini, section: &str, key: &str) -> Option<&'a str> {
    profile::lines(ini, section)
        .into_iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| section_value(v))
}

/// `ReadKeyISectionData`: the number of a present value (C `wcstol`), else `default`.
fn int(ini: &Ini, section: &str, key: &str, default: i64) -> i64 {
    match string(ini, section, key).map(str::trim) {
        Some(v) if !v.is_empty() => i64::from(strtol(v).0),
        _ => default,
    }
}

/// `ReadKeyBSectionData` (or `ReadKeyB`) of a value: YES / NO, TRUE / FALSE, ON / OFF,
/// else the number (0 for text that is none); absent or empty, `default`.
fn boolean(v: Option<&str>, default: bool) -> bool {
    let Some(v) = v.map(|v| v.trim().to_ascii_uppercase()) else {
        return default;
    };
    match v.as_str() {
        "" => default,
        "YES" | "TRUE" | "ON" => true,
        "NO" | "FALSE" | "OFF" => false,
        n => strtol(n).0 != 0,
    }
}

/// A value as BonDriver_BDA keeps a section's lines: trimmed, and a leading `"` (else
/// `'`) removed with everything from the last one of it on.
fn section_value(v: &str) -> &str {
    let v = v.trim();
    for q in ['"', '\''] {
        if let Some(rest) = v.strip_prefix(q) {
            return rest.rfind(q).map_or(rest, |r| &rest[..r]);
        }
    }
    v
}

/// `[Tuner] DefaultNetwork` (`ReadKeyIValueMapSectionData`): a name of the map, else the
/// number (C `wcstol`, text that is none 0); absent or empty, SPHD. A number without a
/// network sets nothing up, as 0 (NONE).
fn network(ini: &Ini) -> Network {
    let v = match string(ini, "Tuner", "DefaultNetwork") {
        Some(v) if !v.is_empty() => v.to_ascii_uppercase(),
        _ => return Network::Sphd,
    };
    match v.as_str() {
        "NONE" => Network::None,
        "SPHD" => Network::Sphd,
        "BS/CS110" | "BS" | "CS110" => Network::BsCs,
        "UHF/CATV" | "UHF" | "CATV" => Network::Uhf,
        "DUAL" => Network::Dual,
        n => match strtol(n).0 {
            1 => Network::Sphd,
            2 => Network::BsCs,
            3 => Network::Uhf,
            4 => Network::Dual,
            _ => Network::None,
        },
    }
}

fn setup(ini: &Ini, net: Network) -> Setup {
    // [Satellite]
    let mut satellite_names = vec![String::new(); MAX_SATELLITE];
    satellite_names[0] = "not set".into();
    let mut lnb = vec![[Lnb::default(); 5]; MAX_SATELLITE];
    let mut auto = vec![String::new(); MAX_SATELLITE];
    match net {
        Network::Sphd => {
            auto[1] = "JCSAT-3".into();
            auto[2] = "JCSAT-4".into();
        }
        Network::BsCs | Network::Dual => auto[1] = "BS/CS110".into(),
        _ => {}
    }
    let (mut bs_cs, mut jcsat3, mut jcsat4) = (1, 1, 2);
    for n in 1..MAX_SATELLITE {
        let p1 = format!("Satellite{n}");
        if let Some(v) = string(ini, "Satellite", &format!("{p1}SettingsAuto")) {
            auto[n] = v.to_ascii_uppercase();
        }
        let both = |l: &mut Lnb, khz| {
            l.low = khz;
            l.high = khz;
        };
        match auto[n].as_str() {
            "JCSAT-3" | "JCSAT-4" => {
                if auto[n] == "JCSAT-3" {
                    jcsat3 = n;
                    satellite_names[n] = "128.0E".into();
                } else {
                    jcsat4 = n;
                    satellite_names[n] = "124.0E".into();
                }
                both(&mut lnb[n][1], 11_200_000);
                both(&mut lnb[n][2], 11_200_000);
            }
            "BS/CS110" => {
                bs_cs = n;
                satellite_names[n] = "BS/CS110".into();
                both(&mut lnb[n][3], 9_505_000);
                both(&mut lnb[n][4], i64::from(LNB_LO_KHZ));
            }
            _ => {}
        }
        if let Some(v) = string(ini, "Satellite", &format!("{p1}Name")) {
            satellite_names[n] = v.to_owned();
        }
        for (p, c) in POLARISATIONS.iter().enumerate().skip(1) {
            let l = &mut lnb[n][p];
            // the setting for every polarisation, then the polarisation's own
            for prefix in [p1.clone(), format!("{p1}{c}")] {
                let o = int(ini, "Satellite", &format!("{prefix}Oscillator"), l.high);
                both(l, o);
                l.high = int(ini, "Satellite", &format!("{prefix}HighOscillator"), l.high);
                l.low = int(ini, "Satellite", &format!("{prefix}LowOscillator"), l.low);
            }
            for prefix in [p1.clone(), format!("{p1}{c}")] {
                l.switch = int(ini, "Satellite", &format!("{prefix}LNBSwitch"), l.switch);
            }
        }
    }

    // [Modulation]
    let mut auto = vec![String::new(); MAX_MODULATION];
    match net {
        Network::Sphd => {
            auto[0] = "DVB-S".into();
            auto[1] = "DVB-S2".into();
        }
        Network::BsCs => auto[0] = "ISDB-S".into(),
        Network::Uhf => auto[0] = "ISDB-T".into(),
        Network::Dual => {
            auto[0] = "ISDB-T".into();
            auto[1] = "ISDB-S".into();
        }
        Network::None => {}
    }
    let mut kinds = vec![Kind::Other("not set".into()); MAX_MODULATION];
    let mut modulation_names = vec![String::new(); MAX_MODULATION];
    let (mut isdb_t, mut isdb_s, mut dvb_s, mut dvb_s2) = (0, 0, 0, 1);
    let (mut qam64, mut qam256, mut opticast) = (0, 1, 0);
    for n in 0..MAX_MODULATION {
        let prefix = format!("ModulationType{n}");
        if let Some(v) = string(ini, "Modulation", &format!("{prefix}SettingsAuto")) {
            auto[n] = v.to_ascii_uppercase();
        }
        let named = match auto[n].as_str() {
            "DVB-S" => {
                dvb_s = n;
                Some(("DVB-S", Kind::Other("DVB-S".into())))
            }
            "DVB-S2" => {
                dvb_s2 = n;
                Some(("DVB-S2", Kind::Other("DVB-S2".into())))
            }
            "ISDB-S" => {
                isdb_s = n;
                Some(("ISDB-S", Kind::IsdbS))
            }
            "ISDB-T" => {
                isdb_t = n;
                Some(("ISDB-T", Kind::IsdbT))
            }
            "J.83C-64QAM" => {
                qam64 = n;
                Some(("J.83C-64QAM", Kind::Other("64QAM".into())))
            }
            "J.83C-256QAM" => {
                qam256 = n;
                Some(("J.83C-256QAM", Kind::Other("256QAM".into())))
            }
            "J.83B-OPTICAST" => {
                opticast = n;
                Some(("J.83B-Opticast", Kind::Other("256QAM".into())))
            }
            "J.83B-64QAM" => Some(("J.83B-64QAM", Kind::Other("64QAM".into()))),
            "J.83B-256QAM" => Some(("J.83B-256QAM", Kind::Other("256QAM".into()))),
            _ => None,
        };
        if let Some((name, kind)) = named {
            modulation_names[n] = name.into();
            kinds[n] = kind;
        }
        if let Some(v) = string(ini, "Modulation", &format!("{prefix}Name")) {
            modulation_names[n] = v.to_owned();
        }
        if let Some(kind) =
            string(ini, "Modulation", &format!("{prefix}Modulation")).and_then(modulation_kind)
        {
            kinds[n] = kind;
        }
    }
    Setup {
        satellite_names,
        lnb,
        kinds,
        modulation_names,
        isdb_t,
        isdb_s,
        dvb_s,
        dvb_s2,
        qam64,
        qam256,
        opticast,
        bs_cs,
        jcsat3,
        jcsat4,
    }
}

/// `ModulationTypeNModulation` (`ReadKeyIValueMapSectionData`): a `ModulationType` name,
/// else its number (C `wcstol`); `None` for an empty value, which keeps the kind before.
fn modulation_kind(v: &str) -> Option<Kind> {
    let v = v.trim().to_ascii_uppercase();
    if v.is_empty() {
        return None;
    }
    Some(match v.as_str() {
        "BDA_MOD_ISDB_T_TMCC" => Kind::IsdbT,
        "BDA_MOD_ISDB_S_TMCC" => Kind::IsdbS,
        _ if v.starts_with("BDA_MOD_") => Kind::Other(v),
        _ => match strtol(&v).0 {
            34 => Kind::IsdbT,
            35 => Kind::IsdbS,
            _ => Kind::Other(v),
        },
    })
}

/// The tuning spaces of a BonDriver_BDA ini.
pub fn parse(ini: &Ini) -> Vec<Space> {
    let net = network(ini);
    let s = setup(ini, net);
    // the one key BonDriver_BDA reads with the profile API (`ReadKeyB`)
    let reserve_all = boolean(profile::get(ini, "Channel", "ReserveUnusedCh"), false);
    let space0_name = string(ini, "Tuner", "TuningSpaceName").unwrap_or("スカパー");
    let mut spaces: BTreeMap<usize, Space> = BTreeMap::new();
    for n in 0..100 {
        let mut section = format!("TuningSpace{n:02}");
        if profile::lines(ini, &section).is_empty() {
            if n != 0 {
                continue;
            }
            section = "Channel".into(); // space 0 is there in any case
        }
        let default_name = if n == 0 { space0_name } else { "NoName" };
        spaces.insert(n, space(ini, &section, default_name, reserve_all, &s));
    }
    if let Some(space0) = spaces.get_mut(&0)
        && space0.channels.is_empty()
        && net == Network::Sphd
    {
        // BonDriver_BDA's three DVB-S transponders for an SPHD space 0 without channels
        for name in [
            "128.0E/12658V/DVB-S",
            "124.0E/12613H/DVB-S2",
            "128.0E/12733H/DVB-S2",
        ] {
            space0.channels.push(Channel {
                name: name.into(),
                tuning: Tuning::Untunable("modulation DVB-S".into()),
            });
        }
    }
    let last = spaces.keys().next_back().copied().unwrap_or(0);
    (0..=last)
        .map(|n| {
            spaces.remove(&n).unwrap_or_else(|| Space {
                name: "-".into(),
                channels: Vec::new(),
            })
        })
        .collect()
}

/// One space from its section.
fn space(ini: &Ini, section: &str, default_name: &str, reserve_all: bool, s: &Setup) -> Space {
    let mut reserve = boolean(string(ini, section, "ReserveUnusedCh"), reserve_all);
    let name = string(ini, section, "TuningSpaceName")
        .unwrap_or(default_name)
        .to_owned();
    let mut chs: BTreeMap<u32, ChData> = BTreeMap::new();

    // ChannelSettingsAuto, then ChannelGenerateN
    let auto = string(ini, section, "ChannelSettingsAuto")
        .unwrap_or("")
        .to_ascii_uppercase();
    let options = string(ini, section, "ChannelSettingsAutoOptions")
        .unwrap_or("")
        .to_ascii_uppercase();
    let mut generate = auto_generate(&auto, &options, s);
    generate.resize(100, String::new());
    let mut next = 0;
    for (i, default) in generate.iter().enumerate() {
        let line = string(ini, section, &format!("ChannelGenerate{i}")).unwrap_or(default);
        if line.is_empty() {
            break;
        }
        if run_generate(line, next, &mut chs, &mut next) {
            reserve = true;
        }
    }

    // CHnnn lines, by key (a repeated one in file order)
    let mut lines: Vec<(String, &str)> = profile::lines(ini, section)
        .into_iter()
        .filter(|(key, _)| is_ch_key(key))
        .map(|(key, value)| (key.to_ascii_uppercase(), section_value(value)))
        .collect();
    lines.sort_by(|a, b| a.0.cmp(&b.0));
    for (key, value) in lines {
        if value.is_empty() {
            continue;
        }
        let number = key[2..].parse().unwrap_or(0);
        let at = if reserve { number } else { chs.len() as u32 };
        let ch = chs.entry(at).or_default();
        ch_line(value, ch, s);
    }

    let offset = int(ini, section, "FrequencyOffset", 0);
    let count = chs.keys().next_back().map_or(0, |&n| n + 1);
    let channels = (0..count)
        .map(|n| match chs.get(&n) {
            Some(ch) => Channel {
                name: ch.name.clone(),
                tuning: tuning(ch, offset, s),
            },
            None => crate::undefined("----"),
        })
        .collect();
    Space { name, channels }
}

/// The `ChannelGenerate` lines `ChannelSettingsAuto` stands for.
fn auto_generate(auto: &str, options: &str, s: &Setup) -> Vec<String> {
    let opts: Vec<&str> = options.split(',').map(str::trim).take(10).collect();
    let has = |o: &str| opts.contains(&o);
    let relative = |prefix: &str| {
        opts.iter()
            .find_map(|o| o.strip_prefix(prefix))
            .unwrap_or("")
            .to_owned()
    };
    let rel = {
        let r = relative("RELATIVETS:");
        if r.is_empty() {
            relative("RELATIVETS-256QAM:")
        } else {
            r
        }
    };
    let rel64 = relative("RELATIVETS-64QAM:");
    let (all, vhf_plus, c24_plus) = (has("ALL"), has("VHF+"), has("C24+"));
    let t = s.isdb_t;
    let mut g = Vec::new();
    let catv = |g: &mut Vec<String>, m: usize, tail: &str, r: &str| {
        let sep = |name: &str| format!("{name}{r},{m}{tail}");
        if all || !vhf_plus {
            g.push(sep("CATV-L"));
        } else {
            g.push(sep("CATV-L:0:9"));
            g.push(sep("CATV-22+"));
        }
        if all || !c24_plus {
            g.push(sep("CATV-H"));
        } else {
            g.push(sep("CATV-H:0:1"));
            g.push(sep("CATV-24+"));
            g.push(sep("CATV-H:5"));
        }
    };
    match auto {
        "UHF" => g.push(if all {
            format!("UHF,{t},,,143")
        } else {
            format!("UHF:0:40,{t},,,143")
        }),
        "CATV" => {
            catv(&mut g, t, ",,,143", "");
            if all {
                g.push(format!("CATV-22+,{t},,,143"));
                g.push(format!("CATV-24+,{t},,,143"));
            }
        }
        "PASSTHROUGH" => {
            g.push(format!("VHF-L,{t},,,143"));
            if all || !vhf_plus {
                g.push(format!("VHF-H,{t},,,143"));
            } else {
                g.push(format!("VHF-4+,{t},,,143"));
                g.push(format!("VHF-H:4,{t},,,143"));
            }
            g.push(format!("UHF,{t},,,143"));
            catv(&mut g, t, ",,,143", "");
            if all {
                g.push(format!("VHF-4+,{t},,,143"));
                g.push(format!("CATV-22+,{t},,,143"));
                g.push(format!("CATV-24+,{t},,,143"));
            }
        }
        "TRANSMODULATION" => {
            // ISDB-C: listed for the numbering only (no QAM on this tuner)
            let mut start = 0;
            let block = |g: &mut Vec<String>, m: usize, r: &str, start: u32| {
                let r = if r.is_empty() {
                    String::new()
                } else {
                    format!(":{r}")
                };
                g.push(format!("VHF-L::{r},{m},,{start},0,,,,1"));
                if all || !vhf_plus {
                    g.push(format!("VHF-H::{r},{m},,,0,,,,1"));
                } else {
                    g.push(format!("VHF-4+::{r},{m},,,0,,,,1"));
                    g.push(format!("VHF-H:4:{r},{m},,,0,,,,1"));
                }
                g.push(format!("UHF::{r},{m},,,0,,,,1"));
                let tail = ",,,0,,,,1";
                if all || !vhf_plus {
                    g.push(format!("CATV-L::{r},{m}{tail}"));
                } else {
                    g.push(format!("CATV-L:0:9{r},{m}{tail}"));
                    g.push(format!("CATV-22+::{r},{m}{tail}"));
                }
                if all || !c24_plus {
                    g.push(format!("CATV-H::{r},{m}{tail}"));
                } else {
                    g.push(format!("CATV-H:0:1{r},{m}{tail}"));
                    g.push(format!("CATV-24+::{r},{m}{tail}"));
                    g.push(format!("CATV-H:5:{r},{m}{tail}"));
                }
                if all {
                    g.push(format!("VHF-4+::{r},{m}{tail}"));
                    g.push(format!("CATV-22+::{r},{m}{tail}"));
                    g.push(format!("CATV-24+::{r},{m}{tail}"));
                }
            };
            if !has("ONLY256QAM") {
                block(&mut g, s.qam64, &rel64, start);
                // in C's unsigned arithmetic, which wraps (MAX_CHANNELS bounds the rest)
                let n = strtol(&rel64).0.max(1) as u32;
                start = start.wrapping_add(n.wrapping_mul(12 + 50 + 51));
                if all {
                    start = start.wrapping_add(n.wrapping_mul(3 + 1 + 4));
                }
                start = start.wrapping_add(99) / 100 * 100;
            }
            if !has("ONLY64QAM") {
                block(&mut g, s.qam256, &rel, start);
            }
        }
        "BS" => {
            let r = if rel.is_empty() {
                String::new()
            } else {
                format!(":{rel}")
            };
            g.push(format!(
                "BS1::{r},{},{},,,BS%02d/TS%d,1,2,0",
                s.isdb_s, s.bs_cs
            ));
        }
        "CS110" => g.push(format!("ND2,{},{},,,ND%02d/TS0,2,2", s.isdb_s, s.bs_cs)),
        "SPHD" => {
            let four = if has("SPINEL") {
                "JCSAT4A-TP%02d"
            } else {
                "JCSAT4B-TP%02d"
            };
            let mut start = 1;
            let mut push = |g: &mut Vec<String>, m: usize| {
                g.push(format!("JD17A,{m},{},{start},,JCSAT3A-TP%02d,1", s.jcsat3));
                g.push(format!("JD1,{m},{},,,JCSAT3A-TP%02d,13", s.jcsat3));
                start += 100;
                g.push(format!("JD17B,{m},{},{start},,{four},1", s.jcsat4));
                g.push(format!("JD1,{m},{},,,{four},17", s.jcsat4));
                start += 100;
            };
            if !has("ONLYHD") {
                push(&mut g, s.dvb_s);
            }
            if !has("ONLYSD") {
                push(&mut g, s.dvb_s2);
            }
        }
        "OPTICAST" => {
            let m = s.opticast;
            let (h11, h26) = (has("H011+"), has("H026+"));
            let lines: &[&str] = if all || (!h11 && !h26) {
                &["OPTICAST"]
            } else if h11 && !h26 {
                &["OPTICAST:0:10", "OPTICAST-11+", "OPTICAST:12"]
            } else if !h11 && h26 {
                &["OPTICAST:0:25", "OPTICAST-26+", "OPTICAST:28"]
            } else {
                &[
                    "OPTICAST:0:10",
                    "OPTICAST-11+",
                    "OPTICAST:12:13",
                    "OPTICAST-26+",
                    "OPTICAST:28",
                ]
            };
            for l in lines {
                g.push(format!("{l},{m},,,0"));
            }
            if all {
                g.push(format!("OPTICAST-11+,{m},,,0"));
                g.push(format!("OPTICAST-26+,{m},,,0"));
            }
        }
        _ => {}
    }
    g
}

/// What a `ChannelGenerate` space makes: how many channels, the name format and
/// numbering, the relative TS count, the first frequency and the step (kHz), and the
/// polarisation (0 none, 1 H / V by turns, 4 R).
struct GenSpace {
    count: u32,
    format: &'static str,
    name_offset: u32,
    name_step: u32,
    relative_ts: u32,
    ts_postfix: bool,
    base: i64,
    step: i64,
    polarisation: u8,
}

fn gen_space(name: &str) -> Option<GenSpace> {
    let g = |count, format, name_offset, base, step| GenSpace {
        count,
        format,
        name_offset,
        name_step: 1,
        relative_ts: 0,
        ts_postfix: true,
        base,
        step,
        polarisation: 0,
    };
    Some(match name {
        "VHF-L" => g(3, "%dch", 1, 93_000, 6_000),
        "VHF-H" => g(9, "%dch", 4, 171_000, 6_000),
        "VHF-4+" => g(4, "%dch+", 4, 173_000, 6_000),
        "UHF" => g(50, "%dch", 13, 473_000, 6_000),
        "CATV-L" => g(10, "C%dch", 13, 111_000, 6_000),
        "CATV-22+" => g(1, "C%dch+", 22, 167_000, 6_000),
        "CATV-H" => g(41, "C%dch", 23, 225_000, 6_000),
        "CATV-24+" => g(4, "C%dch+", 24, 233_000, 6_000),
        "BS1" => GenSpace {
            name_step: 2,
            relative_ts: 4,
            ts_postfix: false,
            polarisation: 4,
            ..g(12, "BS%02d/TS%d", 1, 11_727_480, 38_360)
        },
        "ND2" => GenSpace {
            name_step: 2,
            ts_postfix: false,
            polarisation: 4,
            ..g(12, "ND%02d/TS%d", 2, 12_291_000, 40_000)
        },
        "JD17A" => GenSpace {
            polarisation: 1,
            ..g(12, "JD%d", 17, 12_268_000, 20_000)
        },
        "JD17B" => GenSpace {
            polarisation: 1,
            ..g(16, "JD%d", 17, 12_268_000, 15_000)
        },
        "JD1" => GenSpace {
            polarisation: 1,
            ..g(16, "JD%d", 1, 12_508_000, 15_000)
        },
        "OPTICAST" => g(58, "H%03d", 1, 93_250, 6_500),
        "OPTICAST-11+" => g(2, "H%03d+", 11, 159_250, 6_500),
        "OPTICAST-26+" => g(3, "H%03d+", 26, 259_750, 6_500),
        _ => return None,
    })
}

/// Run one `ChannelGenerate` line,
/// `space[:offset[:count[:relative TS]]],modulation,satellite,first channel,frequency
/// offset,name format,name offset,name step,TS name offset`, into `chs`. True when it
/// made channels (which numbers the space's lines by their number from then on).
fn run_generate(
    line: &str,
    next: u32,
    chs: &mut BTreeMap<u32, ChData>,
    next_out: &mut u32,
) -> bool {
    let f: Vec<&str> = line.split(',').take(9).collect();
    let field = |i: usize| f.get(i).copied().unwrap_or("");
    let num = |s: &str| strtol(s).0;
    let parts: Vec<&str> = field(0).split(':').take(4).collect();
    let part = |i: usize| parts.get(i).copied().unwrap_or("");
    let Some(mut g) = gen_space(&part(0).to_ascii_uppercase()) else {
        return false;
    };
    let offset = num(part(1)) as u32;
    let count = match num(part(2)) {
        c if c < 1 => 999,
        c => c as u32,
    };
    let first = offset.min(g.count - 1);
    let count = count.min(g.count.wrapping_sub(offset));
    let mut format = g.format.to_owned();
    if !part(3).is_empty() {
        g.relative_ts = num(part(3)) as u32;
        if g.ts_postfix {
            format.push_str("/TS%d");
        }
    }
    let modulation = if field(1).is_empty() {
        0
    } else {
        num(field(1)) as usize
    };
    let satellite = if field(2).is_empty() {
        0
    } else {
        num(field(2)) as usize
    };
    let start = if field(3).is_empty() {
        next
    } else {
        num(field(3)) as u32
    };
    let tuning_freq = i64::from(num(field(4)));
    if !field(5).is_empty() {
        format = field(5).to_owned();
    }
    let name_offset = if field(6).is_empty() {
        g.name_offset
    } else {
        num(field(6)) as u32
    };
    let name_step = if field(7).is_empty() {
        g.name_step
    } else {
        num(field(7)) as u32
    };
    let name_offset_ts = if field(8).is_empty() {
        0
    } else {
        num(field(8)) as u32
    };

    if count == 0 {
        return false;
    }
    let mut at = start;
    for ch in first..first.saturating_add(count).min(first + 1000) {
        for ts in 0..g.relative_ts.max(1) {
            if at >= MAX_CHANNELS {
                return true;
            }
            let c = chs.entry(at).or_default();
            c.polarisation = match g.polarisation {
                1 => {
                    if ch % 2 == 1 {
                        1
                    } else {
                        2
                    }
                }
                4 => 4,
                _ => 0,
            };
            c.modulation = modulation;
            c.satellite = satellite;
            c.frequency = g.base + g.step * i64::from(ch) + tuning_freq;
            if g.relative_ts > 0 {
                c.tsid = i64::from(ts);
            }
            c.name = printf(
                &format,
                &[
                    ch.wrapping_mul(name_step).wrapping_add(name_offset),
                    ts.wrapping_add(name_offset_ts),
                ],
            );
            at += 1;
            *next_out = (*next_out).max(at);
        }
    }
    true
}

/// A `CHnnn` line into `ch`: the fields given replace the channel's.
fn ch_line(value: &str, ch: &mut ChData, s: &Setup) {
    let f: Vec<&str> = value.split(',').take(10).collect();
    let field = |i: usize| f.get(i).copied().unwrap_or("");
    if let Ok(n) = usize::try_from(strtol(field(0)).0)
        && n < MAX_SATELLITE
    {
        ch.satellite = n;
    }
    // MHz, with kHz after the point
    if let Some(khz) = mhz_as_khz(field(1))
        && khz > 0
        && khz <= 30_000_000
    {
        ch.frequency = khz;
    }
    let c = field(2).chars().next().unwrap_or(' ');
    if let Some(p) = POLARISATIONS.iter().position(|&p| p == c) {
        ch.polarisation = p;
    }
    if let Ok(n) = usize::try_from(strtol(field(3)).0)
        && n < MAX_MODULATION
    {
        ch.modulation = n;
    }
    ch.name = match field(4) {
        "" => auto_name(ch, s),
        name => name.to_owned(),
    };
    if !field(6).is_empty() {
        ch.tsid = i64::from(strtol(field(6)).0);
    }
}

/// `wcstod(s) * 1000` (rounded: 11727.480 is 11727480).
fn mhz_as_khz(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let end = t
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || (i == 0 && (c == '-' || c == '+'))))
        .map_or(t.len(), |(i, _)| i);
    let v: f64 = t[..end].parse().ok()?;
    Some((v * 1000.0).round() as i64)
}

/// BonDriver_BDA's name for a channel without one (`BS/CS110/11727.480R/ISDB-S`); a
/// number past the tables has no name.
fn auto_name(ch: &ChData, s: &Setup) -> String {
    let (m, k) = (ch.frequency / 1000, ch.frequency % 1000);
    let freq = if k == 0 {
        format!("{m:05}")
    } else {
        format!("{m:05}.{k:03}")
    };
    format!(
        "{}/{freq}{}/{}",
        s.satellite_names
            .get(ch.satellite)
            .map_or("", String::as_str),
        POLARISATIONS[ch.polarisation],
        s.modulation_names
            .get(ch.modulation)
            .map_or("", String::as_str)
    )
}

/// What a channel tunes.
fn tuning(ch: &ChData, offset: i64, s: &Setup) -> Tuning {
    let khz = ch.frequency + offset;
    // a ChannelGenerate line's numbers are not checked (`as isize`: a negative one as given)
    let Some(kind) = s.kinds.get(ch.modulation) else {
        return Tuning::Untunable(format!("modulation number {}", ch.modulation as isize));
    };
    match kind {
        Kind::IsdbT => match u32::try_from(khz) {
            Ok(k) if k > 0 && k < 950_000 => Tuning::Terrestrial { hz: k * 1000 },
            _ => Tuning::Untunable(format!("ISDB-T at {:.3} MHz", khz as f64 / 1000.0)),
        },
        Kind::IsdbS => {
            let Some(lnb) = s.lnb.get(ch.satellite) else {
                return Tuning::Untunable(format!("satellite number {}", ch.satellite as isize));
            };
            let mut lo = lnb[ch.polarisation].oscillator(khz);
            if lo <= 0 && khz > i64::from(LNB_LO_KHZ) {
                lo = i64::from(LNB_LO_KHZ); // no LNB set: BS / CS110's
            }
            let ts = match ch.tsid {
                t if t < 0 => Ts::First,
                t => Ts::from_field(t as u32),
            };
            crate::satellite(khz - lo.max(0), ts)
        }
        Kind::Other(name) => Tuning::Untunable(format!("modulation {name}")),
    }
}

/// The widest field [`printf`] pads to: Rust's formatting takes no more (16 bits), and a
/// channel name this wide is a mistake in the file anyway.
const MAX_WIDTH: usize = 255;

/// C `printf` for the `%d` / `%02d` / `%%` of a channel name format, the values in turn
/// (C's `unsigned int`s: `%d` / `%i` print them signed, `%u` unsigned).
fn printf(format: &str, values: &[u32]) -> String {
    let mut out = String::new();
    let mut values = values.iter();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut spec = String::new();
        while let Some(&d) = chars.peek() {
            if d.is_ascii_digit() || d == '-' || d == 'l' {
                spec.push(d);
                chars.next();
            } else {
                break;
            }
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(conv @ ('d' | 'i' | 'u')) => {
                let v = values.next().copied().unwrap_or(0);
                let v = if conv == 'u' {
                    i64::from(v)
                } else {
                    i64::from(v as i32)
                };
                let spec = spec.replace('l', "");
                let zero = spec.starts_with('0');
                let left = spec.starts_with('-');
                let width: usize = spec.trim_start_matches(['0', '-']).parse().unwrap_or(0);
                let width = width.min(MAX_WIDTH);
                out.push_str(&match (zero, left) {
                    (_, true) => format!("{v:<width$}"),
                    (true, _) => format!("{v:0width$}"),
                    _ => format!("{v:width$}"),
                });
            }
            Some(other) => {
                out.push('%');
                out.push_str(&spec);
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests;
